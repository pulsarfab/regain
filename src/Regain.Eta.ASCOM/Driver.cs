using Regain.SerialServer;
using System.IO;
using System.Collections;
using System.Runtime.InteropServices;
using ASCOM.DeviceInterface;
using Regain.Rotator;

namespace Regain.Eta;

/// One session in the COM local server, shared by separate 32/64-bit applications.
[ComVisible(true), Guid("C12BF695-204B-48B6-B6C6-0B90F238AB7F"), ProgId("ASCOM.Regain.ETA.Focuser"), ClassInterface(ClassInterfaceType.None)]
[ComDefaultInterface(typeof(IFocuserV3))]
public sealed class Driver : IFocuserV3
{
    internal static readonly SharedAccessoryDevice SharedDevice = new(new AccessorySession(
        RegainPaths.EnvironmentVariable("REGAIN_ETA_WORKER") ?? Path.Combine(AppDomain.CurrentDomain.BaseDirectory,"regain-eta.exe"),
        "eta", AccessorySession.SettingsPath("eta","ascom")), AccessorySetupWindow.Show);

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
    public string Name => "PulsarFab regain Wanderer Astro ETA M54";
    public string Description => Name + " over native USB serial";
    public string DriverInfo => "Shared ASCOM local server with an independent Rust serial driver";
    public string DriverVersion => typeof(Driver).Assembly.GetName().Version.ToString();
    public short InterfaceVersion => 3;
    public ArrayList SupportedActions => new() { "Regain.Status", "Regain.Identity", "Regain.MovePoint", "Regain.CancelQueued" };
    public string Action(string ActionName,string ActionParameters) => Regain.Rotator.RegainPaths.ActionName(ActionName) switch {
        "regain.status" => Read(s=>s.Request(new {command="status"}).GetRawText()),
        "regain.identity" => Read(s=>s.Request(new {command="identity"}).GetRawText()),
        "regain.movepoint" => Read(s=>s.EtaMovePoint(ActionParameters)),
        "regain.cancelqueued" => Read(s=>s.Request(new {command="cancel-queued"}).GetRawText()),
        _ => throw new ASCOM.ActionNotImplementedException(ActionName)
    };
    public bool Absolute => Read(_=>true);
    public bool IsMoving => Read(s=>s.Status().Moving);
    public int Position => Read(s=>s.Status().Position);
    public int MaxStep => Read(s=>s.Status().MaxStep);
    public int MaxIncrement => MaxStep;
    public double StepSize => Read(_=>1.0);
    public bool TempCompAvailable => Read(_=>false);
    public bool TempComp { get=>Read(_=>false); set=>Write(_=>throw new ASCOM.PropertyNotImplementedException(nameof(TempComp),true)); }
    public double Temperature => Read(s=>s.Status().Temperature ?? throw new ASCOM.PropertyNotImplementedException(nameof(Temperature),false));
    public void Move(int Position) { int max=MaxStep; if(Position<0 || Position>max) throw new ASCOM.InvalidValueException(nameof(Position),Position.ToString(),"0.."+max); Write(s=>s.Move(Position)); }
    public void Halt()=>Write(_=>throw new ASCOM.MethodNotImplementedException("ETA has no documented stop command"));
    public void SetupDialog() => SharedDevice.Setup();
    public void CommandBlind(string Command,bool Raw)=>throw new ASCOM.MethodNotImplementedException(nameof(CommandBlind));
    public bool CommandBool(string Command,bool Raw)=>throw new ASCOM.MethodNotImplementedException(nameof(CommandBool));
    public string CommandString(string Command,bool Raw)=>throw new ASCOM.MethodNotImplementedException(nameof(CommandString));
    public void Dispose() { if (disposed) return; SharedDevice.Connect(client,false); disposed=true; GC.SuppressFinalize(this); }
}
