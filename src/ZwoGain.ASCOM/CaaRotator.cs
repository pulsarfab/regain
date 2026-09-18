using System.Collections;
using System.Runtime.InteropServices;
using ASCOM.DeviceInterface;
using ZwoGain.Rotator;

namespace ZwoGain.Ascom;

[ComVisible(true), Guid("A918164B-49DD-4FF5-BEE6-A4AB93B97F12"), ProgId("ASCOM.ZWOgain.Rotator"), ClassInterface(ClassInterfaceType.None)]
public sealed class CaaRotator : IRotatorV3, IDisposable
{
    static CaaRotator() => Dependencies.Install();
    private CaaSession? session;
    private bool disposed;
    private CaaSession Session => disposed ? throw new ObjectDisposedException(nameof(CaaRotator)) : session ??= NewSession();
    private static CaaSession NewSession() => new(Environment.GetEnvironmentVariable("ZWOGAIN_CAA_WORKER") ?? Path.Combine(Path.GetDirectoryName(typeof(CaaRotator).Assembly.Location)!, "zwogain-caa.exe"), CaaSession.SettingsPath("ascom"));
    private CaaSession Device => Session.Connected ? Session : throw new ASCOM.NotConnectedException("CAA is disconnected");
    public string Name => "ZWOgain CAA Rotator";
    public string Description => "ZWO CAA rotator over USB HID";
    public string DriverInfo => "ZWOgain native Rust CAA driver";
    public string DriverVersion => typeof(CaaRotator).Assembly.GetName().Version.ToString();
    public short InterfaceVersion => 3;
    public bool Connected { get => session?.Connected == true; set { if (value) Session.Connect(); else session?.Disconnect(); } }
    public bool CanReverse => true;
    public bool Reverse {
        get => Device.Request(new { command = "settings" }).GetProperty("reverse").GetBoolean();
        set { Device.Request(new { command = "reverse", enabled = value }); Device.RememberCoordinates(); }
    }
    public bool IsMoving { get { var s = Device.Status(); Device.CheckMotion(s); return s.Moving; } }
    public float Position => (float)Device.Status().Logical;
    public float MechanicalPosition => (float)CaaSession.Wrap(Device.Status().Mechanical);
    public float TargetPosition => (float)Device.Status().Target;
    public float StepSize => .02f;
    private static void Angle(float angle, bool relative = false) {
        if (float.IsNaN(angle) || float.IsInfinity(angle) || (relative ? Math.Abs(angle) > 360 : angle < 0 || angle >= 360))
            throw new ASCOM.InvalidValueException("Position", angle.ToString(System.Globalization.CultureInfo.InvariantCulture), relative ? "-360..360" : "0..<360");
    }
    public void Move(float Position) { Angle(Position, true); Device.Command("move-relative", Position); }
    public void MoveAbsolute(float Position) { Angle(Position); Device.Command("move-to", Position); }
    public void MoveMechanical(float Position) { Angle(Position); Device.Command("move-mechanical", Position); }
    public void Sync(float Position) { Angle(Position); Device.Sync(Position); }
    public void Halt() => Device.Halt();
    public ArrayList SupportedActions => new(CaaSession.Actions);
    public string Action(string ActionName, string ActionParameters) {
        if (!CaaSession.Actions.Any(a => a.Equals(ActionName, StringComparison.OrdinalIgnoreCase))) throw new ASCOM.ActionNotImplementedException(ActionName);
        return Device.Action(ActionName, ActionParameters);
    }
    public void CommandBlind(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandBlind));
    public bool CommandBool(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandBool));
    public string CommandString(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandString));
    public void SetupDialog() {
        if (Connected) CaaSetupForm.ShowModal(Device);
        else { using (var setup = NewSession()) CaaSetupForm.ShowModal(setup); session?.Dispose(); session = null; }
    }
    public void Dispose() { if (disposed) return; disposed = true; session?.Dispose(); session = null; GC.SuppressFinalize(this); }
}
