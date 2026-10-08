using System.Collections;
using System.Diagnostics;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Windows.Forms;
using ASCOM.Common;
using ASCOM.Common.DeviceInterfaces;
using ConformU;

if (args.Length != 2 || !args[0].StartsWith("Rgn.F") || !args[0].EndsWith(".covercalibrator")) throw new ArgumentException("Private panel fixture only");
var settings = new Settings { DeviceTechnology = DeviceTechnology.COM, DeviceType = DeviceTypes.CoverCalibrator,
    ComDevice = new ComDevice("PRIVATE timing simulation", args[0]) };
using var logger = new ConformLogger("timing", args[1], "timing", false);
using var facade = new FacadeBaseClass(settings, logger);
if (facade.InterfaceVersion != 2 || !facade.Name.Contains("SIMULATION") || facade.Connected) throw new InvalidOperationException("Not an idle private panel simulation");
facade.Connected = true;
var rows = new List<object>();
try {
    for (var i = 0; i < 5; i++) {
        var timer = Stopwatch.StartNew();
        var states = facade.DeviceState;
        var elapsedMs = timer.Elapsed.TotalMilliseconds;
        Validate(states);
        rows.Add(new { phase = "unmodifiedFacade", sample = i, elapsedMs, count = states.Count });
    }
    var flags = BindingFlags.Instance | BindingFlags.NonPublic;
    dynamic driver = typeof(FacadeBaseClass).GetField("Driver", flags)!.GetValue(facade)!;
    var form = (Control)typeof(FacadeBaseClass).GetField("DriverHostForm", flags)!.GetValue(facade)!;
    for (var i = 0; i < 20; i++) {
        var timer = Stopwatch.StartNew();
        IEnumerable raw = (IEnumerable)form.Invoke((Func<IEnumerable>)(() => (IEnumerable)driver.DeviceState));
        var getMs = timer.Elapsed.TotalMilliseconds;
        var values = new List<StateValue>();
        foreach (dynamic item in raw) values.Add(new StateValue(item.Name, item.Value));
        var enumerateMs = timer.Elapsed.TotalMilliseconds - getMs;
        var cleaned = OperationalStateProperty.Clean(values, DeviceTypes.CoverCalibrator, logger);
        var cleanMs = timer.Elapsed.TotalMilliseconds - getMs - enumerateMs;
        Validate(cleaned);
        rows.Add(new { phase = "splitFacade", sample = i, getMs, enumerateMs,
            cleanMs, count = cleaned.Count });
        if (Marshal.IsComObject(raw)) Marshal.FinalReleaseComObject(raw);
    }
} finally { facade.Connected = false; }
Console.WriteLine(JsonSerializer.Serialize(new { runtime = Environment.Version.ToString(), rows }));

static void Validate(List<StateValue> states)
{
    var values = states.ToDictionary(state => state.Name, state => state.Value);
    if (values.Count != 5 || !Equals(values["Brightness"], 0) ||
        !Equals(values["CoverMoving"], false) || !Equals(values["CalibratorChanging"], false) ||
        values["CoverState"].ToString() != "Closed" || values["CalibratorState"].ToString() != "Off")
        throw new InvalidOperationException("Unexpected private panel state");
}
