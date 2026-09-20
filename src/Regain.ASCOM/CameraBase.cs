using System.Collections;
using System.Runtime.InteropServices;
using ASCOM.DeviceInterface;

[assembly: ComVisible(false)]
namespace Regain.Ascom;

[ComVisible(false)]
public abstract partial class CameraBase : ICameraV4, IDisposable
{
    static CameraBase() => Dependencies.Install();
    private readonly int slot;
    private LocalCamera? client;
    private bool disposed;
    private readonly object sync = new();
    protected CameraBase(int slot) { this.slot = slot; }
    private LocalCamera Client { get { lock (sync) { if (disposed) throw new ObjectDisposedException(nameof(CameraBase)); return client ??= new LocalCamera(slot); } } }
    public string Name => $"PulsarFab regain Retryable Camera {slot + 1}";
    public string Description => "ZWO camera driver with automatic retries";
    public string DriverInfo => "PulsarFab regain native Rust camera driver";
    public string DriverVersion => typeof(CameraBase).Assembly.GetName().Version.ToString();
    public short InterfaceVersion => 4;
    public ArrayList SupportedActions => new() { "Regain.Diagnostics", "Regain.Controls", "Regain.SetControl" };
    public bool Connected { get => client?.Get<bool>("connected") ?? false; set { if (value) Client.Put("connected", true); else if (client is not null) { try { client.Put("connected", false); } finally { lock (sync) { client.Dispose(); client = null; } } } } }
    public void Connect() => Client.Put("connect");
    public void Disconnect() { if (client is not null) client.Put("disconnect"); }
    public void SetupDialog() => CameraSetupWindow.Show(Client, slot);
    public IStateValueCollection DeviceState => new StateValueCollection(Client.Request("get", "devicestate").EnumerateArray().Select(v => new StateValue(v.GetProperty("Name").GetString(), v.GetProperty("Value").ValueKind == System.Text.Json.JsonValueKind.True || v.GetProperty("Value").ValueKind == System.Text.Json.JsonValueKind.False ? (object)v.GetProperty("Value").GetBoolean() : v.GetProperty("Value").GetInt32())).ToList());
    public string Action(string ActionName, string ActionParameters) {
        ActionName = Regain.Rotator.RegainPaths.ActionName(ActionName);
        if (!SupportedActions.Cast<string>().Any(a => a.Equals(ActionName, StringComparison.OrdinalIgnoreCase))) throw new ASCOM.ActionNotImplementedException(ActionName);
        return Client.Request("put", "action", new { Action = ActionName, Parameters = ActionParameters }).GetString()!;
    }
    public void CommandBlind(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandBlind));
    public bool CommandBool(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandBool));
    public string CommandString(string Command, bool Raw) => throw new ASCOM.MethodNotImplementedException(nameof(CommandString));
    public void AbortExposure() => Client.Put("abortexposure");
    public void StopExposure() => throw new ASCOM.MethodNotImplementedException(nameof(StopExposure));
    public void StartExposure(double Duration, bool Light) {
        if (double.IsNaN(Duration) || double.IsInfinity(Duration)) throw new ASCOM.InvalidValueException("Duration must be finite");
        Client.Request("put", "startexposure", new { Duration, Light });
    }
    public void PulseGuide(GuideDirections Direction, int Duration) => throw new ASCOM.MethodNotImplementedException(nameof(PulseGuide));
    public void Dispose() { lock (sync) { if (disposed) return; disposed = true; client?.Dispose(); client = null; } GC.SuppressFinalize(this); }
    ~CameraBase() { Task.Run(() => { try { Dispose(); } catch { } }); }
}

[ComVisible(true), Guid("D1DB6F94-5CC0-4752-A758-F849098874A1"), ProgId("ASCOM.ZWOgain.Camera1"), ClassInterface(ClassInterfaceType.None)]
public sealed class Camera1() : CameraBase(0);
[ComVisible(true), Guid("D1DB6F94-5CC0-4752-A758-F849098874A2"), ProgId("ASCOM.ZWOgain.Camera2"), ClassInterface(ClassInterfaceType.None)]
public sealed class Camera2() : CameraBase(1);
[ComVisible(true), Guid("D1DB6F94-5CC0-4752-A758-F849098874A3"), ProgId("ASCOM.ZWOgain.Camera3"), ClassInterface(ClassInterfaceType.None)]
public sealed class Camera3() : CameraBase(2);
[ComVisible(true), Guid("D1DB6F94-5CC0-4752-A758-F849098874A4"), ProgId("ASCOM.ZWOgain.Camera4"), ClassInterface(ClassInterfaceType.None)]
public sealed class Camera4() : CameraBase(3);
