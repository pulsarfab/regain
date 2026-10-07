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
            if (settings.TryGetProperty("rawBytesHex", out var hexadecimal)) {
                var text = hexadecimal.GetString()!;
                var binary = Enumerable.Range(0,text.Length / 2).Select(index => Convert.ToByte(text.Substring(index * 2,2),16)).ToArray();
                output.Write(binary,0,binary.Length);
            }
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
    private readonly Dictionary<string, object> cameraValues = new(StringComparer.Ordinal);
    private bool cameraReady;
    private double cameraDuration;
    private int captures;
    private readonly Stopwatch cameraGuide = new();
    private int cameraGuideMilliseconds;
    private object CameraValue(string name, object fallback) {
        Before(name);
        if (Settings().TryGetProperty("camera" + name, out var value)) return value.ValueKind switch {
            JsonValueKind.True => true, JsonValueKind.False => false, JsonValueKind.String => value.GetString()!,
            JsonValueKind.Number => value.TryGetInt32(out var integer) ? (object)integer : value.GetDouble(),
            JsonValueKind.Array => value.EnumerateArray().Select(item => item.ValueKind == JsonValueKind.String ? (object)item.GetString()! : item.GetInt32()).ToArray(),
            _ => new object()
        };
        return cameraValues.TryGetValue(name, out var stored) ? stored : fallback;
    }
    private void CameraSet(string name, object value) {
        Before(name + ".set", value); cameraValues[name] = value;
        Record("Applied." + name,value);
        if (Setting("cameraLostReply",false)) Thread.Sleep(600000);
    }
    public object BayerOffsetX { get => CameraValue("BayerOffsetX", 0); }
    public object BayerOffsetY { get => CameraValue("BayerOffsetY", 0); }
    public object BinX { get => CameraValue("BinX", (short)1); set => CameraSet("BinX", value); }
    public object BinY { get => CameraValue("BinY", (short)1); set => CameraSet("BinY", value); }
    public object CameraState { get => CameraValue("CameraState", 0); }
    public object CameraXSize { get => CameraValue("CameraXSize", 320); }
    public object CameraYSize { get => CameraValue("CameraYSize", 240); }
    public object CanAbortExposure { get => CameraValue("CanAbortExposure", true); }
    public object CanAsymmetricBin { get => CameraValue("CanAsymmetricBin", true); }
    public object CanFastReadout { get => CameraValue("CanFastReadout", true); }
    public object CanGetCoolerPower { get => CameraValue("CanGetCoolerPower", true); }
    public object CanPulseGuide { get => CameraValue("CanPulseGuide", true); }
    public object CanSetCCDTemperature { get => CameraValue("CanSetCCDTemperature", true); }
    public object CanStopExposure { get => CameraValue("CanStopExposure", true); }
    public object CCDTemperature { get => CameraValue("CCDTemperature", -10.0); }
    public object CoolerOn { get => CameraValue("CoolerOn", false); set => CameraSet("CoolerOn", value); }
    public object CoolerPower { get => CameraValue("CoolerPower", 12.5); }
    public object ElectronsPerADU { get => CameraValue("ElectronsPerADU", 0.5); }
    public object ExposureMin { get => CameraValue("ExposureMin", 0.001); }
    public object ExposureMax { get => CameraValue("ExposureMax", 600.0); }
    public object ExposureResolution { get => CameraValue("ExposureResolution", 0.001); }
    public object FastReadout { get => CameraValue("FastReadout", false); set => CameraSet("FastReadout", value); }
    public object FullWellCapacity { get => CameraValue("FullWellCapacity", 20000.0); }
    public object Gain { get => CameraValue("Gain", (short)0); set => CameraSet("Gain", value); }
    public object GainMin { get => CameraValue("GainMin", -10); }
    public object GainMax { get => CameraValue("GainMax", 100); }
    public object Gains { get => CameraValue("Gains", new System.Collections.ArrayList { "Low", "High" }); }
    public object HasShutter { get => CameraValue("HasShutter", true); }
    public object HeatSinkTemperature { get => CameraValue("HeatSinkTemperature", 15.0); }
    public object ImageReady { get => CameraValue("ImageReady", cameraReady); }
    public object IsPulseGuiding { get => CameraValue("IsPulseGuiding", cameraGuide.IsRunning && cameraGuide.ElapsedMilliseconds < cameraGuideMilliseconds); }
    public object LastExposureDuration { get => CameraValue("LastExposureDuration", cameraDuration); }
    public object LastExposureStartTime { get => CameraValue("LastExposureStartTime", "2026-10-07T01:02:03.1234567Z"); }
    public object MaxADU { get => CameraValue("MaxADU", 65535); }
    public object MaxBinX { get => CameraValue("MaxBinX", 4); }
    public object MaxBinY { get => CameraValue("MaxBinY", 4); }
    public object NumX { get => CameraValue("NumX", 320); set => CameraSet("NumX", value); }
    public object NumY { get => CameraValue("NumY", 240); set => CameraSet("NumY", value); }
    public object Offset { get => CameraValue("Offset", 0); set => CameraSet("Offset", value); }
    public object OffsetMin { get => CameraValue("OffsetMin", 0); }
    public object OffsetMax { get => CameraValue("OffsetMax", 100); }
    public object Offsets { get => CameraValue("Offsets", new[]{"Low", "High"}); }
    public object PercentCompleted { get => CameraValue("PercentCompleted", 0); }
    public object PixelSizeX { get => CameraValue("PixelSizeX", 3.76); }
    public object PixelSizeY { get => CameraValue("PixelSizeY", 3.76); }
    public object ReadoutMode { get => CameraValue("ReadoutMode", (short)0); set => CameraSet("ReadoutMode", value); }
    public object ReadoutModes { get => CameraValue("ReadoutModes", new System.Collections.ArrayList { "Normal", "Fast" }); }
    public object SensorName { get => CameraValue("SensorName", "Private COM camera"); }
    public object SensorType { get => CameraValue("SensorType", 0); }
    public object SetCCDTemperature { get => CameraValue("SetCCDTemperature", 0.0); set => CameraSet("SetCCDTemperature", value); }
    public object StartX { get => CameraValue("StartX", 0); set => CameraSet("StartX", value); }
    public object StartY { get => CameraValue("StartY", 0); set => CameraSet("StartY", value); }
    public object SubExposureDuration { get => CameraValue("SubExposureDuration", 0.0); set => CameraSet("SubExposureDuration", value); }

    public void StartExposure(double duration, bool light) { Before("StartExposure", new { duration, light }); cameraDuration = duration; captures++; cameraReady = true; }
    public void StopExposure() { Before("StopExposure"); cameraReady = true; }
    public void AbortExposure() { Before("AbortExposure"); cameraReady = false; }
    public void PulseGuide(global::ASCOM.DeviceInterface.GuideDirections direction, int duration) {
        Before("PulseGuide",new { direction=(int)direction,duration });
        cameraGuideMilliseconds=duration; cameraGuide.Restart();
        Record("Applied.PulseGuide",new { direction=(int)direction,duration });
        if (Setting("cameraBlockingGuide",false)) { Thread.Sleep(duration); cameraGuide.Stop(); }
        if (Setting("cameraLostReply",false)) Thread.Sleep(600000);
    }
    public object ImageArray {
        get {
            Before("ImageArray");
            var settings = Settings();
            var name = settings.TryGetProperty("imageType", out var selected) ? selected.GetString() : "int32";
            var type = name switch { "int16" => typeof(short), "int32" => typeof(int), "double" => typeof(double), "single" => typeof(float),
                "uInt64" => typeof(ulong), "byte" => typeof(byte), "int64" => typeof(long), "uInt16" => typeof(ushort), "uInt32" => typeof(uint), _ => typeof(string) };
            var width = settings.TryGetProperty("imageWidth", out var x) ? x.GetInt32() : Convert.ToInt32(cameraValues.TryGetValue("NumX", out var nx) ? nx : 320);
            var height = settings.TryGetProperty("imageHeight", out var y) ? y.GetInt32() : Convert.ToInt32(cameraValues.TryGetValue("NumY", out var ny) ? ny : 240);
            var planes = settings.TryGetProperty("imagePlanes", out var z) ? z.GetInt32() : 0;
            var dimensions = planes == 0 ? new[]{width,height} : new[]{width,height,planes};
            if (settings.TryGetProperty("imageRank", out var rank) && rank.GetInt32() != dimensions.Length) dimensions = Enumerable.Repeat(2, rank.GetInt32()).ToArray();
            var lower = Setting("imageLowerBounds",false) ? -7 : 0;
            var array = Array.CreateInstance(type, dimensions, Enumerable.Repeat(lower,dimensions.Length).ToArray());
            if (dimensions.Length is not (2 or 3)) return array;
            for (var a = 0; a < width; a++) for (var b = 0; b < height; b++) for (var c = 0; c < Math.Max(1,planes); c++) {
                var index = (a * height + b) * Math.Max(1,planes) + c + captures;
                object value = name switch {
                    "int16" => (short)(-index % 30000), "int32" => -index, "double" => index + 0.25,
                    "single" => (float)(index + 0.5), "uInt64" => (1UL << 63) + (ulong)index,
                    "byte" => (byte)(index % 256), "int64" => long.MinValue + index,
                    "uInt16" => (ushort)(40000 + index % 20000), "uInt32" => 0x80000000U + (uint)index, _ => "bad"
                };
                array.SetValue(value, planes == 0 ? new[]{a+lower,b+lower} : new[]{a+lower,b+lower,c+lower});
            }
            return array;
        }
    }

}
