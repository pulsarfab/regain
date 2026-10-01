using System.Collections;
using System.Runtime.InteropServices;
using ASCOM.DeviceInterface;
using Regain.Rotator;
using Regain.SerialServer;
using System.IO;

namespace Regain.FocusCube;

[ComVisible(true), Guid("D2F1A72E-1038-4BFA-BDF1-F1ED7BD3F67B"), ProgId("ASCOM.PulsarFab.Regain.FalconV2.Rotator"), ClassInterface(ClassInterfaceType.None)]
[ComDefaultInterface(typeof(IRotatorV3))]
public sealed class FalconDriver : IRotatorV3, IDisposable
{
    internal static readonly SharedDevice<CaaSession> SharedDevice = new(new CaaSession(
        RegainPaths.EnvironmentVariable("REGAIN_FALCON_WORKER") ?? Path.Combine(AppDomain.CurrentDomain.BaseDirectory,"regain-device.exe"),
        CaaSession.SettingsPath("falcon-ascom"), true), (s,clients) => CaaSetupWindow.Show(s, false, clients, DisconnectSetup));
    private static void DisconnectSetup() => SharedDevice.DisconnectSetup();
    private readonly Guid client=Guid.NewGuid();
    private bool disposed;
    public FalconDriver() => Program.Track(this);
    ~FalconDriver() { try { SharedDevice.Connect(client,false); } catch { } }
    private T Read<T>(Func<CaaSession,T> action) {
        lock (SharedDevice.Gate) {
            if (disposed) throw new ObjectDisposedException(Name);
            if (!Connected) throw new ASCOM.NotConnectedException(Name);
            try { return action(SharedDevice.Session); }
            catch (ASCOM.DriverException) { throw; }
            catch (Exception e) { throw new ASCOM.DriverException(e.Message,e); }
        }
    }
    private void Write(Action<CaaSession> action) => Read(s => { action(s); return true; });
    public string Name => "PulsarFab regain Pegasus Falcon V2";
    public string Description => "Pegasus Falcon V2 rotator over USB serial";
    public string DriverInfo => "PulsarFab regain shared native Rust Pegasus driver";
    public string DriverVersion => typeof(FalconDriver).Assembly.GetName().Version.ToString();
    public short InterfaceVersion => 3;
    public bool Connected { get => !disposed && SharedDevice.Connected(client); set { if(disposed) throw new ObjectDisposedException(Name); SharedDevice.Connect(client,value); } }
    public bool CanReverse => Read(_ => true);
    public bool Reverse {
        get => Read(s => s.Request(new { command = "settings" }).GetProperty("reverse").GetBoolean());
        set => Write(s => { s.Request(new { command = "reverse", enabled = value }); s.RememberCoordinates(); });
    }
    public bool IsMoving => Read(d => { var s = d.Status(); d.CheckMotion(s); return s.Moving; });
    public float Position => Read(s => (float)s.Status().Logical);
    public float MechanicalPosition => Read(s => (float)CaaSession.Wrap(s.Status().Mechanical));
    public float TargetPosition => Read(s => (float)s.Status().Target);
    public float StepSize => Read(_ => .01f);
    private static void Angle(float angle, bool relative = false) {
        if (float.IsNaN(angle) || float.IsInfinity(angle) || (relative ? Math.Abs(angle) > 360 : angle < 0 || angle >= 360))
            throw new ASCOM.InvalidValueException("Position", angle.ToString(System.Globalization.CultureInfo.InvariantCulture), relative ? "-360..360" : "0..<360");
    }
    public void Move(float Position) { Angle(Position, true); Write(s => s.Command("move-relative", Position)); }
    public void MoveAbsolute(float Position) { Angle(Position); Write(s => s.Command("move-to", Position)); }
    public void MoveMechanical(float Position) { Angle(Position); Write(s => s.Command("move-mechanical", Position)); }
    public void Sync(float Position) { Angle(Position); Write(s => s.Sync(Position)); }
    public void Halt() => Write(s => s.Halt());
    public ArrayList SupportedActions => new(SharedDevice.Session.SupportedActions);
    public string Action(string ActionName, string ActionParameters) {
        if (!SharedDevice.Session.SupportedActions.Any(a => a.Equals(ActionName, StringComparison.OrdinalIgnoreCase))) throw new ASCOM.ActionNotImplementedException(ActionName);
        return Read(s => s.Action(ActionName, ActionParameters));
    }
    public void CommandBlind(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandBlind));
    public bool CommandBool(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandBool));
    public string CommandString(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandString));
    public void SetupDialog() => SharedDevice.Setup();
    public void Dispose() { if(disposed) return; SharedDevice.Connect(client,false); disposed=true; GC.SuppressFinalize(this); }
}
