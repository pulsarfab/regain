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
    private int position = 50;
    private bool moving, tempComp;
    private readonly bool rotator;
    private readonly bool wheel;
    private short wheelPosition;
    private double logical = 20, mechanical = 350, target = 20;
    private bool reverse;
    private int brightness, coverState = 1, calibratorState = 1;
    private bool coverMoving, calibratorChanging;
    public Driver() {
        var explicitState = Environment.GetEnvironmentVariable("REGAIN_HUB_COM_FIXTURE_STATE");
        var arguments = Environment.GetCommandLineArgs();
        var type = Array.IndexOf(arguments, "--device-type");
        rotator = type >= 0 && arguments[type + 1] == "rotator";
        wheel = type >= 0 && arguments[type + 1] == "filterwheel";
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
    private object PanelValue(string member, object fallback) {
        Before(member);
        if (!Settings().TryGetProperty("panel" + member, out var value)) return fallback;
        return value.ValueKind switch {
            JsonValueKind.True => true, JsonValueKind.False => false,
            JsonValueKind.String => value.GetString()!,
            JsonValueKind.Number when value.TryGetInt32(out var integer) => integer,
            JsonValueKind.Number when value.TryGetInt64(out var wide) => wide,
            JsonValueKind.Number => value.GetDouble(), _ => new object()
        };
    }
    public object Brightness => PanelValue("Brightness", brightness);
    public object MaxBrightness => PanelValue("MaxBrightness", 4096);
    public object CoverState => PanelValue("CoverState", (ASCOM.DeviceInterface.CoverStatus)coverState);
    public object CalibratorState => PanelValue("CalibratorState", (ASCOM.DeviceInterface.CalibratorStatus)calibratorState);
    private void RequireModernPanel() {
        if (Settings().TryGetProperty("version", out var version) && version.GetInt32() == 1)
            throw new COMException("Legacy completion", unchecked((int)0x80040400));
    }
    public object CoverMoving { get { RequireModernPanel(); return PanelValue("CoverMoving", coverMoving); } }
    public object CalibratorChanging { get { RequireModernPanel(); return PanelValue("CalibratorChanging", calibratorChanging); } }
    public void OpenCover() { Before("OpenCover"); coverState = 2; coverMoving = true; }
    public void CloseCover() { Before("CloseCover"); coverState = 2; coverMoving = true; }
    public void HaltCover() { Before("HaltCover"); coverState = 4; coverMoving = false; }
    public void CalibratorOn(int value) {
        Before("CalibratorOn", value); brightness = value; calibratorState = 2; calibratorChanging = true;
        Record("CalibratorOn.applied", value);
        if (Setting("panelLoseOnReply", false)) Thread.Sleep(600000);
        if (Setting("panelFaultAfterOn", false)) throw new ArgumentException("PRIVATE_FIXTURE_SECRET_DO_NOT_ECHO");
    }
    public void CalibratorOff() { Before("CalibratorOff"); brightness = 0; calibratorState = 1; calibratorChanging = false; }
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
    public bool Absolute { get { Before("Absolute"); return !Setting("relative", false); } }
    public object MaxStep { get { Before("MaxStep"); return Setting("badMaxStep", false) ? (object)1.5 : 100000; } }
    public int MaxIncrement { get { Before("MaxIncrement"); return 1000; } }
    public object Position {
        get {
            Before("Position");
            if (wheel) return Setting("badWheelPosition",false) ? (object)1.5 : wheelPosition;
            if (rotator) return Setting("badAngle", false) ? 360.0 : logical;
            if (Setting("relative", false)) throw new COMException("relative", unchecked((int)0x80040400));
            return position;
        }
        set {
            Before("Position.set",value);
            if (!wheel || value is not short slot) throw new ArgumentException("Wheel setter requires Short");
            if (slot >= 3) throw new COMException("slot",unchecked((int)0x80040401));
            wheelPosition = Setting("wheelMoving",false) ? (short)-1 : slot;
        }
    }
    public object Names {
        get {
            Before("Names"); var settings = Settings();
            if (settings.TryGetProperty("wheelNamesCase",out var choice)) return choice.GetString() switch {
                "empty" => Array.Empty<string>(), "nested" => new string[1,1], "wrong" => new object[]{1},
                "many" => Enumerable.Repeat("L",1025).ToArray(), "large" => new[]{new string('α',524289)},
                "unicode" => new[]{"\uD800"}, _ => new[]{"L","Hα",""}
            };
            return new[]{"L","Hα",""};
        }
    }
    public object FocusOffsets {
        get {
            Before("FocusOffsets"); var settings = Settings();
            if (settings.TryGetProperty("wheelOffsetsCase",out var choice)) return choice.GetString() switch {
                "empty" => Array.Empty<int>(), "noZero" => new[]{1,2,3}, "fraction" => new object[]{0,1.5},
                "string" => new object[]{0,"1"}, "overflow" => new object[]{0,2147483648L},
                "mismatch" => new[]{0}, "boundaries" => new[]{int.MinValue,0,int.MaxValue}, _ => new[]{-12,0,17}
            };
            return new[]{-12,0,17};
        }
    }
    public object IsMoving { get { Before("IsMoving"); return Setting("badMoving", false) ? (object)"false" : moving; } }
    public bool TempCompAvailable { get { Before("TempCompAvailable"); return true; } }
    public bool TempComp { get { Before("TempComp.get"); return tempComp; } set { Before("TempComp.set", value); tempComp = value; } }
    public double StepSize { get { Before("StepSize"); return Setting("badStepSize", false) ? 0 : Setting("tinyStepSize", false) ? double.Epsilon : rotator ? 0.02 : 1.25; } }
    public void Move(object value) {
        Before("Move", value);
        if (rotator) target = Wrap(logical + Convert.ToDouble(value));
        else position = Convert.ToInt32(value);
        moving = true;
    }
    private static double Wrap(double value) => (value % 360 + 360) % 360;
    public object CanReverse { get { Before("CanReverse"); return Setting("badReverse", false) ? (object)"true" : !Setting("noReverse", false); } }
    public object Reverse { get { Before("Reverse.get"); return Setting("badReverse", false) ? (object)"false" : reverse; } set { Before("Reverse.set", value); reverse = (bool)value; } }
    public double MechanicalPosition { get { Before("MechanicalPosition"); return mechanical; } }
    public double TargetPosition { get { Before("TargetPosition"); return target; } }
    public void MoveAbsolute(float value) { Before("MoveAbsolute", value); target = value; moving = true; }
    public void MoveMechanical(float value) { Before("MoveMechanical", value); target = Wrap(logical + value - mechanical); mechanical = value; moving = true; }
    public void Sync(float value) { Before("Sync", value); logical = target = value; }
    public void Halt() { Before("Halt"); moving = false; }
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
