using System.Collections;
using System.Runtime.InteropServices;
using ASCOM.DeviceInterface;
using Regain.Rotator;

namespace Regain.Ascom;

[ComVisible(false)]
public abstract class AccessoryBase : IDisposable
{
    private AccessorySession? session;
    private bool disposed;
    protected abstract string Kind { get; }
    public abstract string Name { get; }
    public abstract short InterfaceVersion { get; }
    static AccessoryBase() => Dependencies.Install();
    private AccessorySession NewSession() => new(Regain.Rotator.RegainPaths.EnvironmentVariable("REGAIN_ACCESSORY_WORKER") ?? Path.Combine(Path.GetDirectoryName(typeof(AccessoryBase).Assembly.Location)!, "regain-device.exe"), Kind, AccessorySession.SettingsPath(Kind, "ascom"));
    private AccessorySession Session => disposed ? throw new ObjectDisposedException(Name) : session ??= NewSession();
    protected AccessorySession Device => Session.Connected ? Session : throw new ASCOM.NotConnectedException(Name + " is disconnected");
    protected T Read<T>(Func<AccessorySession, T> operation)
    {
        try { return operation(Device); }
        catch (ASCOM.DriverException) { throw; }
        catch (Exception error) { throw new ASCOM.DriverException(error.Message, error); }
    }
    protected void Write(Action<AccessorySession> operation) => Read(d => { operation(d); return true; });
    public bool Connected { get => session?.Connected == true; set { if (value) Session.Connect(); else session?.Disconnect(); } }
    public string Description => Name + " over native USB HID";
    public string DriverInfo => "PulsarFab regain SDK-free Rust USB driver";
    public string DriverVersion => typeof(AccessoryBase).Assembly.GetName().Version.ToString();
    public ArrayList SupportedActions => Kind == "efw" ? new() { "Regain.Status", "Regain.Identity", "Regain.Calibrate" } : new() { "Regain.Status", "Regain.Identity" };
    public string Action(string ActionName, string ActionParameters) => Regain.Rotator.RegainPaths.ActionName(ActionName) switch {
        "regain.status" => Read(d => d.Request(new { command = "status" }).GetRawText()),
        "regain.identity" => Read(d => d.Request(new { command = "identity" }).GetRawText()),
        "regain.calibrate" when Kind == "efw" => Read(d => { d.Calibrate(); return "null"; }),
        _ => throw new ASCOM.ActionNotImplementedException(ActionName)
    };
    public void CommandBlind(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandBlind));
    public bool CommandBool(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandBool));
    public string CommandString(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandString));
    public void SetupDialog()
    {
        if (Connected) AccessorySetupWindow.Show(Device);
        else { using (var setup = NewSession()) AccessorySetupWindow.Show(setup); session?.Dispose(); session = null; }
    }
    public void Dispose() { if (disposed) return; disposed = true; session?.Dispose(); session = null; GC.SuppressFinalize(this); }
}

[ComVisible(true), Guid("EA2040E1-E936-4BDF-87F7-B58CA3E418AB"), ProgId("ASCOM.ZWOgain.FilterWheel"), ClassInterface(ClassInterfaceType.None)]
public sealed class EfwFilterWheel : AccessoryBase, IFilterWheelV2
{
    protected override string Kind => "efw";
    public override string Name => "PulsarFab regain EFW Filter Wheel";
    public override short InterfaceVersion => 2;
    public string[] Names => Read(d => (string[])d.Profile.Names.Clone());
    public int[] FocusOffsets => Read(d => (int[])d.Profile.FocusOffsets.Clone());
    public short Position {
        get => Read(d => { var s = d.Status(); return s.Moving ? (short)-1 : checked((short)s.Position); });
        set {
            int count = Read(d => d.Status().Slots);
            if (value < 0 || value >= count) throw new ASCOM.InvalidValueException(nameof(Position), value.ToString(), "0.." + (count - 1));
            Write(d => d.Move(value));
        }
    }
}

[ComVisible(true), Guid("295C08F8-EDE9-43C5-9D55-627A063D74CA"), ProgId("ASCOM.ZWOgain.Focuser"), ClassInterface(ClassInterfaceType.None)]
public sealed class EafFocuser : AccessoryBase, IFocuserV3
{
    protected override string Kind => "eaf";
    public override string Name => "PulsarFab regain EAF Focuser";
    public override short InterfaceVersion => 3;
    public bool Absolute => Read(_ => true);
    public bool Link { get => Connected; set => Connected = value; }
    public bool IsMoving => Read(d => d.Status().Moving);
    public int MaxStep => Read(d => d.Status().MaxStep);
    public int MaxIncrement => MaxStep;
    public int Position => Read(d => d.Status().Position);
    public double StepSize => Read<double>(_ => throw new ASCOM.PropertyNotImplementedException(nameof(StepSize), false));
    public bool TempCompAvailable => Read(_ => false);
    public bool TempComp { get => Read(_ => false); set { _ = Device; throw new ASCOM.PropertyNotImplementedException(nameof(TempComp), true); } }
    public double Temperature => Read(d => d.Status().Temperature ?? throw new ASCOM.PropertyNotImplementedException(nameof(Temperature), false));
    public void Move(int Position)
    {
        int maximum = MaxStep;
        if (Position < 0 || Position > maximum) throw new ASCOM.InvalidValueException(nameof(Position), Position.ToString(), "0.." + maximum);
        Write(d => d.Move(Position));
    }
    public void Halt() => Write(d => d.Halt());
}
