using System.IO;
using System.Collections;
using System.Runtime.InteropServices;
using ASCOM.DeviceInterface;
using ZwoGain.Rotator;

namespace ZwoGain.FocusCube;

/// One session in the COM local server, shared by separate 32/64-bit applications.
internal static class SharedDevice
{
    internal static readonly object Gate = new();
    internal static readonly AccessorySession Session = new(
        Environment.GetEnvironmentVariable("ZWOGAIN_FC3_WORKER") ?? Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "zwogain-fc3.exe"),
        "fc3", AccessorySession.SettingsPath("fc3", "ascom"));
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
    internal static void Setup() {
        lock (Gate) { if (setupActive) throw new InvalidOperationException("Setup is already open"); setupActive=true; }
        try { AccessorySetupWindow.Show(Session, () => { lock(Gate) return Clients.Count > 0; }); }
        finally { lock (Gate) { setupActive=false; if (Clients.Count == 0) Session.Disconnect(); } }
    }
}

[ComVisible(true), Guid("69AB224B-14D2-46A2-A744-0C60593A28B3"), ProgId("ASCOM.ZWOgain.FocusCube3.Focuser"), ClassInterface(ClassInterfaceType.None)]
[ComDefaultInterface(typeof(IFocuserV3))]
public sealed class Driver : IFocuserV3
{
    private readonly Guid client = Guid.NewGuid();
    private bool disposed;
    public Driver() => Program.Track(this);
    ~Driver() { try { SharedDevice.Connect(client, false); } catch { } }
    private T Read<T>(Func<AccessorySession,T> action) {
        lock (SharedDevice.Gate) {
            if (disposed) throw new ObjectDisposedException(Name);
            if (!Connected) throw new ASCOM.NotConnectedException(Name);
            try { return action(SharedDevice.Session); }
            catch (ASCOM.DriverException) { throw; }
            catch (Exception e) { throw new ASCOM.DriverException(e.Message,e); }
        }
    }
    private void Write(Action<AccessorySession> action) => Read(s => { action(s); return true; });
    public bool Connected { get => !disposed && SharedDevice.Connected(client); set { if (disposed) throw new ObjectDisposedException(Name); SharedDevice.Connect(client,value); } }
    public bool Link { get=>Connected; set=>Connected=value; }
    public string Name => "ZWOgain Pegasus FocusCube3";
    public string Description => Name + " over native USB serial";
    public string DriverInfo => "Shared ASCOM local server with an independent Rust serial driver";
    public string DriverVersion => typeof(Driver).Assembly.GetName().Version.ToString();
    public short InterfaceVersion => 3;
    public ArrayList SupportedActions => new() { "ZwoGain.Status", "ZwoGain.Identity" };
    public string Action(string ActionName,string ActionParameters) => ActionName.ToLowerInvariant() switch {
        "zwogain.status" => Read(s=>s.Request(new {command="status"}).GetRawText()),
        "zwogain.identity" => Read(s=>s.Request(new {command="identity"}).GetRawText()),
        _ => throw new ASCOM.ActionNotImplementedException(ActionName)
    };
    public bool Absolute => Read(_=>true);
    public bool IsMoving => Read(s=>s.Status().Moving);
    public int Position => Read(s=>s.Status().Position);
    public int MaxStep => Read(s=>s.Status().MaxStep);
    public int MaxIncrement => MaxStep;
    public double StepSize => Read<double>(_=>throw new ASCOM.PropertyNotImplementedException(nameof(StepSize),false));
    public bool TempCompAvailable => Read(_=>false);
    public bool TempComp { get=>Read(_=>false); set=>Write(_=>throw new ASCOM.PropertyNotImplementedException(nameof(TempComp),true)); }
    public double Temperature => Read(s=>s.Status().Temperature ?? throw new ASCOM.PropertyNotImplementedException(nameof(Temperature),false));
    public void Move(int Position) { int max=MaxStep; if(Position<0 || Position>max) throw new ASCOM.InvalidValueException(nameof(Position),Position.ToString(),"0.."+max); Write(s=>s.Move(Position)); }
    public void Halt()=>Write(s=>s.Halt());
    public void SetupDialog() => SharedDevice.Setup();
    public void CommandBlind(string Command,bool Raw)=>throw new ASCOM.MethodNotImplementedException(nameof(CommandBlind));
    public bool CommandBool(string Command,bool Raw)=>throw new ASCOM.MethodNotImplementedException(nameof(CommandBool));
    public string CommandString(string Command,bool Raw)=>throw new ASCOM.MethodNotImplementedException(nameof(CommandString));
    public void Dispose() { if (disposed) return; SharedDevice.Connect(client,false); disposed=true; GC.SuppressFinalize(this); }
}
