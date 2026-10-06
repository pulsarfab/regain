using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Threading;
using System.Windows.Threading;

namespace Regain.Hub.COM.Fixture;

// Only the fixture reads these environment variables. The production worker has
// no simulator or fixture bypass: tests activate this class via the COM registry.
[ComVisible(true), Guid("E86CDFE1-0282-4A40-9265-F7D6A2CEBAF1"), ClassInterface(ClassInterfaceType.AutoDual)]
public sealed class Driver {
    private readonly string state;
    private readonly string trace;
    private bool connected;
    private bool pumped;
    private double level;
    private double average;
    private int pending;
    public Driver() {
        var explicitState = Environment.GetEnvironmentVariable("REGAIN_HUB_COM_FIXTURE_STATE");
        var arguments = Environment.GetCommandLineArgs();
        var selected = Array.IndexOf(arguments, "--prog-id");
        state = explicitState ?? Path.Combine(Environment.GetEnvironmentVariable("REGAIN_HUB_COM_FIXTURE_DIRECTORY")
            ?? throw new InvalidOperationException(), arguments[selected + 1] + ".json");
        trace = state + ".trace";
        Record("Activate");
        connected = Setting("initialConnected", false);
        var helper = Environment.GetEnvironmentVariable("REGAIN_HUB_COM_FIXTURE_HELPER");
        if (helper != null && Setting("spawnHelper", false)) {
            var process = Process.Start(new ProcessStartInfo {
                FileName = helper, Arguments = "\"" + state + ".stop\"",
                UseShellExecute = false, CreateNoWindow = true, WindowStyle = ProcessWindowStyle.Hidden
            }) ?? throw new InvalidOperationException();
            Record("SharedHelper", process.Id);
            process.Dispose();
        }
        Dispatcher.CurrentDispatcher.BeginInvoke(new Action(() => { pumped = true; Record("Pumped"); }));
    }
    private JsonElement Settings() {
        using var stream = new FileStream(state, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
        using var doc = JsonDocument.Parse(stream);
        return doc.RootElement.Clone();
    }
    private bool Setting(string name, bool fallback) => Settings().TryGetProperty(name, out var value) ? value.GetBoolean() : fallback;
    private void Record(string member, object? value = null) {
        File.AppendAllText(trace, JsonSerializer.Serialize(new {
            member, value, pid = Process.GetCurrentProcess().Id, bitness = IntPtr.Size * 8,
            thread = Thread.CurrentThread.ManagedThreadId, apartment = Thread.CurrentThread.GetApartmentState().ToString()
        }) + "\n");
    }
    private void Before(string member, object? value = null) {
        Record(member, value);
        var settings = Settings();
        if (settings.TryGetProperty("rawReplyMember", out var rawMember) && rawMember.GetString() == member) {
            var bytes = System.Text.Encoding.UTF8.GetBytes(settings.GetProperty("rawFrame").GetString() + "\n");
            using var output = Console.OpenStandardOutput();
            output.Write(bytes, 0, bytes.Length);
            output.Flush();
        }
        if (settings.TryGetProperty("hangMember", out var hang) && hang.GetString() == member) Thread.Sleep(600000);
        if (settings.TryGetProperty("argumentFaultMember", out var argument) && argument.GetString() == member)
            throw new ArgumentException("PRIVATE_FIXTURE_SECRET_DO_NOT_ECHO");
        if (settings.TryGetProperty("faultMember", out var fault) && fault.GetString() == member)
            throw new COMException("PRIVATE_FIXTURE_SECRET_DO_NOT_ECHO", settings.GetProperty("faultCode").GetInt32());
    }
    public short InterfaceVersion { get { Before("InterfaceVersion"); return Settings().TryGetProperty("version", out var value) ? value.GetInt16() : (short)3; } }
    public bool Connected { get { Before("Connected.get"); return connected && !Setting("verifyDisconnected", false); } set { Before("Connected.set", value); connected = value; } }
    public bool Connecting {
        get {
            Before("Connecting");
            if (!pumped) throw new COMException("STA message pump did not run", unchecked((int)0x800404FF));
            return Setting("connectingForever", false) || pending-- > 0;
        }
    }
    public void Connect() { Before("Connect"); connected = true; pending = 2; }
    public void Disconnect() { Before("Disconnect"); connected = Setting("sharedConnected", false); pending = 2; }
    public string Name { get { Before("Name"); Console.WriteLine("vendor stdout noise"); return "COM fixture"; } }
    public string Description { get { Before("Description"); return "Isolated ASCOM fixture"; } }
    public string DriverInfo { get { Before("DriverInfo"); return "Private test driver"; } }
    public string DriverVersion => "1.0";
    public object IsSafe { get { Before("IsSafe"); return Setting("badSafe", false) ? "true" : (object)Setting("safe", true); } }
    public short MaxSwitch { get { Before("MaxSwitch"); return 2; } }
    public bool GetSwitch(short id) { Before("GetSwitch", id); return level != 0; }
    public double GetSwitchValue(short id) { Before("GetSwitchValue", id); return level; }
    public string GetSwitchName(short id) { Before("GetSwitchName", id); return "Fixture level"; }
    public string GetSwitchDescription(short id) { Before("GetSwitchDescription", id); return "Fixture gauge"; }
    public bool CanWrite(short id) { Before("CanWrite", id); return id == 0; }
    public double MinSwitchValue(short id) { Before("MinSwitchValue", id); return 0; }
    public double MaxSwitchValue(short id) { Before("MaxSwitchValue", id); return 100; }
    public double SwitchStep(short id) { Before("SwitchStep", id); return 0.5; }
    public void SetSwitch(short id, bool value) { Before("SetSwitch", new { id, value }); level = value ? 1 : 0; }
    public void SetSwitchValue(short id, double value) {
        // Record a dispatch before the lost-reply fixture stalls; its count
        // survives worker termination and proves the parent did not replay it.
        Before("SetSwitchValue", new { id, value }); level = value;
    }
    public double AveragePeriod { get { Before("AveragePeriod.get"); return average; } set { Before("AveragePeriod.set", value); average = value; } }
    public double Temperature { get { Before("Temperature"); return 12.5; } }
    public double Humidity { get { Before("Humidity"); return 45; } }
    public double DewPoint { get { Before("DewPoint"); return 0.5; } }
    public double CloudCover { get { Before("CloudCover"); return 0; } }
    public double Pressure { get { Before("Pressure"); return 1001; } }
    public double RainRate { get { Before("RainRate"); return 0; } }
    public double SkyBrightness { get { Before("SkyBrightness"); return 17; } }
    public double SkyQuality { get { Before("SkyQuality"); return 20; } }
    public double SkyTemperature { get { Before("SkyTemperature"); return -10; } }
    public double StarFWHM { get { Before("StarFWHM"); return 2; } }
    public double WindDirection { get { Before("WindDirection"); return 120; } }
    public double WindGust { get { Before("WindGust"); return 3; } }
    public double WindSpeed { get { Before("WindSpeed"); return 2; } }
    public double TimeSinceLastUpdate(string sensor) { Before("TimeSinceLastUpdate", sensor); return 3.5; }
    public string SensorDescription(string sensor) { Before("SensorDescription", sensor); return "Fixture " + sensor; }
    public void Refresh() { Before("Refresh"); }
    public void SetupDialog() { Before("SetupDialog"); throw new InvalidOperationException("Must never be called by polling"); }
    public void Dispose() { Before("Dispose"); connected = false; }
}
