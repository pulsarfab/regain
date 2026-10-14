using System.Diagnostics;
using System.Globalization;
using System.Reflection;
using System.Runtime.ExceptionServices;
using System.Runtime.InteropServices;
using NINA.Core.Utility;
using Regain.Core;

namespace Regain.NINA;

// GSS's standard FindHome is a coordinate slew. Sensor homing is a separate
// server API, not an ASCOM Action or a second serial connection. See the pinned
// upstream API/behavior references in docs/ascom-mount-recovery.md.
internal interface IGreenSwampApi : IDisposable
{
    bool AscomOn { get; set; }
    bool IsMountRunning { get; }
    bool IsConnected { get; }
    bool IsParked { get; }
    bool CanHomeSensors { get; }
    bool IsAutoHomeRunning { get; }
    string LastAutoHomeError { get; }
    bool IsFullStop(int axis);
    void AutoHomeStart(int degreeLimit, int offsetDec);
    void AutoHomeStop();
}

internal sealed class GreenSwampApi(object server) : IGreenSwampApi
{
    internal static IGreenSwampApi Connect()
    {
        var type = Type.GetTypeFromProgID("GS.SkyApi", throwOnError: false)
            ?? throw new IOException("Green Swamp sensor homing requires its installed GS.SkyApi COM interface.");
        return new GreenSwampApi(Activator.CreateInstance(type) ?? throw new IOException("Could not open GS.SkyApi."));
    }
    private object? Invoke(string name, BindingFlags operation, params object[] args)
    {
        try { return server.GetType().InvokeMember(name, operation, null, server, args, CultureInfo.InvariantCulture); }
        catch (TargetInvocationException error) when (error.InnerException is { } inner) {
            ExceptionDispatchInfo.Capture(inner).Throw(); throw;
        }
    }
    private bool ReadBool(string name) => Invoke(name, BindingFlags.GetProperty) is bool value
        ? value : throw new IOException("GS.SkyApi returned an invalid " + name + " value.");
    public bool AscomOn { get => ReadBool(nameof(AscomOn)); set => Invoke(nameof(AscomOn), BindingFlags.SetProperty, value); }
    public bool IsMountRunning => ReadBool(nameof(IsMountRunning));
    public bool IsConnected => ReadBool(nameof(IsConnected));
    public bool IsParked => ReadBool(nameof(IsParked));
    public bool CanHomeSensors => ReadBool(nameof(CanHomeSensors));
    public bool IsAutoHomeRunning => ReadBool(nameof(IsAutoHomeRunning));
    public string LastAutoHomeError
    {
        get {
            try {
                return Invoke(nameof(LastAutoHomeError), BindingFlags.GetProperty) is string value
                    ? value : throw new IOException("GS.SkyApi returned an invalid AutoHome error value.");
            } catch (Exception error) when (error is NullReferenceException ||
                error is COMException { HResult: unchecked((int)0x80004003) }) {
                // GSS 1.2.2.7 clears its exception field to null on start, but
                // this getter dereferences .Message. COM translates that exact
                // null-reference bug to E_POINTER. No other API error is ignored.
                // The caller separately requires start acknowledgement, stopped
                // axes, AtHome, no slew and a connected server before success.
                Logger.Warning("Regain GSS AutoHome: LastAutoHomeError has GSS's null-error getter bug; verifying stopped/home state.");
                return string.Empty;
            }
        }
    }
    public bool IsFullStop(int axis) => Invoke(nameof(IsFullStop), BindingFlags.InvokeMethod, axis) is bool value
        ? value : throw new IOException("GS.SkyApi returned an invalid axis stop value.");
    public void AutoHomeStart(int degreeLimit, int offsetDec) => Invoke(nameof(AutoHomeStart), BindingFlags.InvokeMethod, degreeLimit, offsetDec);
    public void AutoHomeStop() => Invoke(nameof(AutoHomeStop), BindingFlags.InvokeMethod);
    public void Dispose() { if (Marshal.IsComObject(server)) Marshal.FinalReleaseComObject(server); }
}

internal static class GreenSwampAutoHome
{
    internal static Task RunAsync(RegainMount mount, MountRecoveryOptions options, Func<IGreenSwampApi> connect,
        Action guard, Action<string> report, CancellationToken token)
    {
        // Keep creation, calls and release on one apartment. The task remains
        // pending until the real asynchronous server operation stops, even when
        // RecoverAsync's outer deadline has already returned to the sequencer.
        var result = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var thread = new Thread(() => {
            try { Run(mount, options, connect, guard, report, token); result.SetResult(); }
            catch (Exception error) { result.SetException(error); }
        }) { IsBackground = true, Name = "Regain GSS AutoHome" };
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();
        return result.Task;
    }

    private static void Run(RegainMount mount, MountRecoveryOptions options, Func<IGreenSwampApi> connect,
        Action guard, Action<string> report, CancellationToken token)
    {
        guard();
        using var api = connect();
        bool blocked = false, started = false, completed = false;
        void CheckServer()
        {
            guard();
            if (!api.IsMountRunning || !api.IsConnected || api.IsParked)
                throw new IOException("GSS disconnected, stopped or parked during sensor homing.");
            if (blocked && api.AscomOn) throw new IOException("GSS ASCOM movement was re-enabled during sensor homing.");
            guard();
        }
        void Pause() { token.WaitHandle.WaitOne(250); guard(); }
        try {
            CheckServer();
            if (api.IsAutoHomeRunning || !api.AscomOn || mount.Slewing)
                throw new IOException("GSS already has an AutoHome/motion operation or ASCOM movement is disabled; inspect it before recovery.");
            if (!api.CanHomeSensors)
                throw new IOException("This GSS mount does not support home sensors. Sensor AutoHome will not fall back to a coordinate-only home slew.");
            guard();
            mount.TrackingEnabled = false;
            var stopTracking = Stopwatch.StartNew();
            while (mount.TrackingEnabled || !api.IsFullStop(1) || !api.IsFullStop(2)) {
                CheckServer();
                if (stopTracking.Elapsed.TotalSeconds >= 5) throw new IOException("GSS did not stop tracking and both axes before sensor homing.");
                Pause();
            }
            guard();
            // Prevent other ASCOM clients from moving during the sensor search.
            // AutoHomeStart stops tracking inside GSS; writing Tracking through
            // ASCOM after this gate closes would be silently ignored by GSS.
            blocked = true;
            api.AscomOn = false;
            if (api.AscomOn) throw new IOException("GSS did not disable ASCOM movement for sensor homing.");
            CheckServer();
            report($"GSS sensor AutoHome: search {options.GreenSwampSearchDegrees}°, Dec offset {options.GreenSwampDeclinationOffsetDegrees}°");
            guard();
            started = true; // A failed COM return can still have started motion.
            api.AutoHomeStart(options.GreenSwampSearchDegrees, options.GreenSwampDeclinationOffsetDegrees);
            if (!api.IsAutoHomeRunning) throw new IOException("GSS did not acknowledge sensor AutoHome starting; it will not be replayed.");
            do { CheckServer(); Pause(); } while (api.IsAutoHomeRunning);
            CheckServer();
            string error = api.LastAutoHomeError;
            if (!string.IsNullOrWhiteSpace(error)) throw new IOException("GSS sensor AutoHome failed: " + error);
            var confirmation = Stopwatch.StartNew();
            while (true) {
                CheckServer();
                if (api.IsAutoHomeRunning) throw new IOException("Another GSS sensor home started before recovery completed.");
                if (api.IsFullStop(1) && api.IsFullStop(2) && mount.AtHome && !mount.Slewing) break;
                if (confirmation.Elapsed.TotalSeconds >= 5) throw new IOException("GSS sensor AutoHome ended without confirmed stopped axes and home position.");
                Pause();
            }
            CheckServer();
            completed = true;
            report("GSS sensor AutoHome completed and home position verified");
        } finally {
            if (started && !completed) {
                // Cancellation does not cancel the server's async-void homing.
                // Request its explicit stop, then retain wrapper/journal
                // ownership until server termination AND axis stop are known.
                try { api.AutoHomeStop(); }
                catch (Exception error) { Logger.Error("Regain GSS AutoHome stop request failed: " + error); }
                try { report("Waiting for GSS sensor homing and both axes to stop; recovery remains fenced"); }
                catch (Exception error) { Logger.Error("Regain GSS stop progress reporting failed: " + error); }
                bool logged = false;
                while (true) {
                    try {
                        if (!api.IsAutoHomeRunning && api.IsMountRunning && api.IsConnected && api.IsFullStop(1) && api.IsFullStop(2)) break;
                    } catch (Exception error) {
                        if (!logged) { Logger.Error("Regain cannot yet confirm GSS sensor homing stopped: " + error); logged = true; }
                    }
                    Thread.Sleep(250);
                }
            }
            if (blocked) {
                api.AscomOn = true;
                if (!api.AscomOn) throw new IOException("GSS did not restore ASCOM movement after sensor homing; inspect server state.");
            }
        }
    }
}
