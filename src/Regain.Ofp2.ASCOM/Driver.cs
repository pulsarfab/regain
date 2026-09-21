using System.IO;
using System.Collections;
using System.Runtime.InteropServices;
using ASCOM.DeviceInterface;
using Regain.Rotator;

namespace Regain.Ofp2;

// The local COM server owns one worker, while each COM object holds its own lease.
internal static class SharedDevice
{
    internal static readonly object Gate = new();
    internal static readonly AccessorySession Session = new(
        RegainPaths.EnvironmentVariable("REGAIN_OFP2_WORKER") ?? Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "regain-ofp2.exe"),
        "ofp2", AccessorySession.SettingsPath("ofp2", "ascom"));
    private static readonly HashSet<Guid> Clients = [];
    private static bool setupActive;
    internal static bool Connected(Guid id) { lock (Gate) return Clients.Contains(id) && Session.Connected; }
    internal static void Connect(Guid id, bool value)
    {
        lock (Gate) {
            if (value) { if (!Session.Connected) { Clients.Clear(); Session.Connect(); } Clients.Add(id); }
            else { Clients.Remove(id); if (Clients.Count == 0 && !setupActive) Session.Disconnect(); }
        }
    }
    internal static void Setup()
    {
        lock (Gate) { if (setupActive) throw new ASCOM.InvalidOperationException("Setup is already open"); setupActive = true; }
        try { Ofp2SetupWindow.Show(Session, () => { lock (Gate) return Clients.Count > 0; }); }
        finally { lock (Gate) { setupActive = false; if (Clients.Count == 0) Session.Disconnect(); } }
    }
}

[ComVisible(true), Guid("8E24512B-6BC6-4A44-9488-53E63C68CCB7"), ProgId("ASCOM.Regain.OFP2.CoverCalibrator"), ClassInterface(ClassInterfaceType.None)]
[ComDefaultInterface(typeof(ICoverCalibratorV1))]
public sealed class Driver : ICoverCalibratorV1
{
    private readonly Guid client = Guid.NewGuid();
    private bool disposed;
    public Driver() => Program.Track(this);
    ~Driver() { try { SharedDevice.Connect(client, false); } catch { } }
    private T Read<T>(Func<AccessorySession, T> action)
    {
        lock (SharedDevice.Gate) {
            if (disposed) throw new ObjectDisposedException(Name);
            if (!Connected) throw new ASCOM.NotConnectedException(Name);
            return Execute(() => action(SharedDevice.Session));
        }
    }
    private static T Execute<T>(Func<T> action)
    {
        try { return action(); }
        catch (ASCOM.DriverException) { throw; }
        catch (Exception e) { throw new ASCOM.DriverException(e.Message, e); }
    }
    private void Command(string command) => Read(s => s.Request(new { command }));
    public bool Connected {
        get => !disposed && SharedDevice.Connected(client);
        set { lock (SharedDevice.Gate) { if (disposed) throw new ObjectDisposedException(Name); Execute(() => { SharedDevice.Connect(client, value); return true; }); } }
    }
    public string Name => "PulsarFab regain Deep Sky Dad OFP2";
    public string Description => Name + " cover and flat panel over native USB serial";
    public string DriverInfo => "Shared ASCOM local server with an independent Rust serial driver";
    public string DriverVersion => typeof(Driver).Assembly.GetName().Version.ToString(2);
    public short InterfaceVersion => 1;
    public ArrayList SupportedActions => new() { "Regain.Status", "Regain.Identity" };
    public string Action(string ActionName, string ActionParameters) => RegainPaths.ActionName(ActionName) switch {
        "regain.status" => Read(s => s.Request(new { command = "status" }).GetRawText()),
        "regain.identity" => Read(s => s.Request(new { command = "identity" }).GetRawText()),
        _ => throw new ASCOM.ActionNotImplementedException(ActionName)
    };
    public int Brightness => Read(s => { var status = Ofp2Status.Read(s); return status.CalibratorOn ? status.Brightness : 0; });
    public int MaxBrightness => Read(s => Ofp2Status.Read(s).MaxBrightness);
    public CalibratorStatus CalibratorState => Read(s => {
        try { return Ofp2Status.Read(s).CalibratorOn ? CalibratorStatus.Ready : CalibratorStatus.Off; }
        catch { return CalibratorStatus.Error; }
    });
    public CoverStatus CoverState => Read(s => {
        try { return Ofp2Status.Read(s).Cover switch {
            "open" => CoverStatus.Open, "closed" => CoverStatus.Closed, "moving" => CoverStatus.Moving, _ => CoverStatus.Unknown
        }; }
        catch { return CoverStatus.Error; }
    });
    public void CalibratorOn(int Brightness) => Read(s => {
        int max = Ofp2Status.Read(s).MaxBrightness;
        if (Brightness < 0 || Brightness > max) throw new ASCOM.InvalidValueException(nameof(Brightness), Brightness.ToString(), "0.." + max);
        return s.Request(new { command = "on", brightness = Brightness });
    });
    public void CalibratorOff() => Command("off");
    public void OpenCover() => Command("open");
    public void CloseCover() => Command("close");
    public void HaltCover() => Command("halt");
    public void SetupDialog() { if (disposed) throw new ObjectDisposedException(Name); SharedDevice.Setup(); }
    public void CommandBlind(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandBlind));
    public bool CommandBool(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandBool));
    public string CommandString(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandString));
    public void Dispose() { lock (SharedDevice.Gate) { if (disposed) return; SharedDevice.Connect(client, false); disposed = true; GC.SuppressFinalize(this); } }
}
