using System.Collections;
using System.Runtime.InteropServices;
using ASCOM.DeviceInterface;

namespace Regain.Ascom;

[ComVisible(false)]
public abstract class QhyFilterWheelBase : IFilterWheelV2, IDisposable
{
    private readonly int slot;
    private LocalCamera? client;
    private bool disposed;
    static QhyFilterWheelBase() => Dependencies.Install();
    protected QhyFilterWheelBase(int slot) => this.slot = slot;
    private LocalCamera Client => disposed ? throw new ObjectDisposedException(Name) : client ??= new LocalCamera(slot, wheel: true);
    public string Name => $"PulsarFab regain QHY Filter Wheel (Camera {slot + 1})";
    public string Description => "QHY 4-pin wheel sharing the matching Regain camera connection";
    public string DriverInfo => Description;
    public string DriverVersion => typeof(QhyFilterWheelBase).Assembly.GetName().Version.ToString();
    public short InterfaceVersion => 2;
    public bool Connected { get => client?.Get<bool>("connected") ?? false; set { if (value) Client.Put("connected", true); else { var old = client; client = null; old?.Dispose(); } } }
    public short Position { get => Client.Get<short>("position"); set => Client.Put("position", value); }
    public string[] Names => Client.Get<string[]>("names");
    public int[] FocusOffsets => Client.Get<int[]>("focusoffsets");
    public ArrayList SupportedActions => new() { "Regain.Diagnostics" };
    public string Action(string ActionName, string ActionParameters) {
        if (!ActionName.Equals("Regain.Diagnostics", StringComparison.OrdinalIgnoreCase)) throw new ASCOM.ActionNotImplementedException(ActionName);
        return Client.Request("put", "action", new { Action = ActionName, Parameters = ActionParameters }).GetString()!;
    }
    public void SetupDialog() => CameraSetupWindow.Show(Client, slot);
    public void CommandBlind(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandBlind));
    public bool CommandBool(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandBool));
    public string CommandString(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandString));
    public void Dispose() { if (disposed) return; disposed = true; var old = client; client = null; old?.Dispose(); GC.SuppressFinalize(this); }
    ~QhyFilterWheelBase() { Task.Run(() => { try { Dispose(); } catch { } }); }
}
[ComVisible(true), Guid("C99D52F5-97D8-4A80-9943-26554DC84511"), ProgId("ASCOM.Regain.QhyFilterWheel1"), ClassInterface(ClassInterfaceType.None)]
public sealed class QhyFilterWheel1() : QhyFilterWheelBase(0);
[ComVisible(true), Guid("C99D52F5-97D8-4A80-9943-26554DC84512"), ProgId("ASCOM.Regain.QhyFilterWheel2"), ClassInterface(ClassInterfaceType.None)]
public sealed class QhyFilterWheel2() : QhyFilterWheelBase(1);
[ComVisible(true), Guid("C99D52F5-97D8-4A80-9943-26554DC84513"), ProgId("ASCOM.Regain.QhyFilterWheel3"), ClassInterface(ClassInterfaceType.None)]
public sealed class QhyFilterWheel3() : QhyFilterWheelBase(2);
[ComVisible(true), Guid("C99D52F5-97D8-4A80-9943-26554DC84514"), ProgId("ASCOM.Regain.QhyFilterWheel4"), ClassInterface(ClassInterfaceType.None)]
public sealed class QhyFilterWheel4() : QhyFilterWheelBase(3);
