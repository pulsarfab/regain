using System.Collections;
using System.Runtime.InteropServices;
using ASCOM.DeviceInterface;
using Regain.Rotator;

namespace Regain.Ascom;

[ComVisible(true), Guid("C99D52F5-97D8-4A80-9943-26554DC84520"), ProgId("ASCOM.Regain.QhyUsbFilterWheel"), ClassInterface(ClassInterfaceType.None)]
public sealed class QhyUsbFilterWheel : IFilterWheelV2, IDisposable
{
    private static readonly Lazy<SharedAccessoryDevice> Shared = new(() => new(
        new AccessorySession(RegainPaths.EnvironmentVariable("REGAIN_ACCESSORY_WORKER") ??
            Path.Combine(Path.GetDirectoryName(typeof(QhyUsbFilterWheel).Assembly.Location)!, "regain-device.exe"),
            "cfw", AccessorySession.SettingsPath("cfw", "ascom")), AccessorySetupWindow.Show));
    private readonly Guid client = Guid.NewGuid();
    private bool disposed;
    static QhyUsbFilterWheel() => Dependencies.Install();
    private T Read<T>(Func<AccessorySession,T> read) {
        lock (Shared.Value.Gate) {
            if (disposed) throw new ObjectDisposedException(Name);
            if (!Connected) throw new ASCOM.NotConnectedException(Name);
            try { return read(Shared.Value.Session); }
            catch (ASCOM.DriverException) { throw; }
            catch (Exception e) { throw new ASCOM.DriverException(e.Message, e); }
        }
    }
    public string Name => "PulsarFab regain QHY Filter Wheel (USB)";
    public string Description => "QHY CFW3 over standalone USB serial";
    public string DriverInfo => "Native Rust serial driver; shared connection within this client application";
    public string DriverVersion => typeof(QhyUsbFilterWheel).Assembly.GetName().Version.ToString();
    public short InterfaceVersion => 2;
    public bool Connected {
        get => !disposed && Shared.Value.Connected(client);
        set { lock (Shared.Value.Gate) { if (disposed) throw new ObjectDisposedException(Name); Shared.Value.Connect(client, value); } }
    }
    public string[] Names => Read(s => (string[])s.Profile.Names.Clone());
    public int[] FocusOffsets => Read(s => (int[])s.Profile.FocusOffsets.Clone());
    public short Position {
        get => Read(s => { var state=s.Status(); return state.Moving ? (short)-1 : checked((short)state.Position); });
        set => Read(s => { if (value < 0 || value >= s.Status().Slots) throw new ASCOM.InvalidValueException(nameof(Position), value.ToString(), "Valid wheel slot"); s.Move(value); return true; });
    }
    public ArrayList SupportedActions => new() { "Regain.Status", "Regain.Identity" };
    public string Action(string ActionName, string ActionParameters) => RegainPaths.ActionName(ActionName) switch {
        "regain.status" => Read(s => s.Request(new { command="status" }).GetRawText()),
        "regain.identity" => Read(s => s.Request(new { command="identity" }).GetRawText()),
        _ => throw new ASCOM.ActionNotImplementedException(ActionName)
    };
    public void SetupDialog() { if (disposed) throw new ObjectDisposedException(Name); Shared.Value.Setup(); }
    public void CommandBlind(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandBlind));
    public bool CommandBool(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandBool));
    public string CommandString(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandString));
    public void Dispose() { lock (Shared.Value.Gate) { if (disposed) return; Shared.Value.Connect(client, false); disposed=true; GC.SuppressFinalize(this); } }
    ~QhyUsbFilterWheel() { Task.Run(() => { try { Dispose(); } catch { } }); }
}
