using System.Collections;
using System.Diagnostics;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Windows.Forms;
using ASCOM.Common;
using ASCOM.Common.DeviceInterfaces;
using ConformU;

if (args.Length != 3 || !new[] { "facadeFirst", "splitFirst" }.Contains(args[2]) || !args[0].StartsWith("Rgn.F") || !args[0].EndsWith(".covercalibrator")) throw new ArgumentException("Private panel fixture only");
var settings = new Settings { DeviceTechnology = DeviceTechnology.COM, DeviceType = DeviceTypes.CoverCalibrator,
    ComDevice = new ComDevice("PRIVATE timing simulation", args[0]) };
using var logger = new ConformLogger("timing", args[1], "timing", false);
using var facade = new FacadeBaseClass(settings, logger);
if (facade.InterfaceVersion != 2 || !facade.Name.Contains("SIMULATION") || facade.Connected) throw new InvalidOperationException("Not an idle private panel simulation");
facade.Connected = true;
var rows = new List<object>();
using var process = Process.GetCurrentProcess();
object? readiness = null;
try {
    // Connected acknowledges the handshake, not completion of the first cache
    // poll. Wait on source telemetry, without calling/warming DeviceState.
    readiness = WaitForSample(args[1]).GetAwaiter().GetResult();
    var flags = BindingFlags.Instance | BindingFlags.NonPublic;
    dynamic driver = typeof(FacadeBaseClass).GetField("Driver", flags)!.GetValue(facade)!;
    var form = (Control)typeof(FacadeBaseClass).GetField("DriverHostForm", flags)!.GetValue(facade)!;
    // Separate fresh processes reverse the order so the split getter,
    // enumeration and cleaning are also measured before any DeviceState use.
    if (args[2] == "splitFirst") ReadSplit(-1);
    for (var i = 0; i < 5; i++) {
        var cpuBefore = process.TotalProcessorTime;
        var gc0 = GC.CollectionCount(0); var gc1 = GC.CollectionCount(1); var gc2 = GC.CollectionCount(2);
        var timer = Stopwatch.StartNew();
        var states = facade.DeviceState;
        var elapsedMs = timer.Elapsed.TotalMilliseconds;
        var gc0Delta = GC.CollectionCount(0) - gc0; var gc1Delta = GC.CollectionCount(1) - gc1;
        var gc2Delta = GC.CollectionCount(2) - gc2;
        var clientCpuMs = (process.TotalProcessorTime - cpuBefore).TotalMilliseconds;
        Validate(states);
        rows.Add(new { phase = "unmodifiedFacade", sample = i, elapsedMs, clientCpuMs,
            gc0 = gc0Delta, gc1 = gc1Delta, gc2 = gc2Delta, count = states.Count });
    }
    for (var i = 0; i < 20; i++) ReadSplit(i);
    void ReadSplit(int i) {
        var timer = Stopwatch.StartNew();
        IEnumerable raw = (IEnumerable)form.Invoke((Func<IEnumerable>)(() => (IEnumerable)driver.DeviceState));
        var getMs = timer.Elapsed.TotalMilliseconds;
        var values = new List<StateValue>();
        foreach (dynamic item in raw) values.Add(new StateValue(item.Name, item.Value));
        var enumerateMs = timer.Elapsed.TotalMilliseconds - getMs;
        var cleaned = OperationalStateProperty.Clean(values, DeviceTypes.CoverCalibrator, logger);
        var cleanMs = timer.Elapsed.TotalMilliseconds - getMs - enumerateMs;
        Validate(cleaned);
        rows.Add(new { phase = i == -1 ? "coldSplit" : "splitFacade", sample = i, getMs, enumerateMs,
            cleanMs, count = cleaned.Count });
        if (Marshal.IsComObject(raw)) Marshal.FinalReleaseComObject(raw);
    }
} catch {
    try { Console.Error.WriteLine("Failure-time source snapshot: " + SourceSnapshot(args[1]).GetAwaiter().GetResult()); }
    catch (Exception error) { Console.Error.WriteLine("Failure snapshot could not be read: " + error.Message); }
    throw;
} finally { facade.Connected = false; }
Console.WriteLine(JsonSerializer.Serialize(new { runtime = Environment.Version.ToString(), order = args[2], readiness, rows }));

static void Validate(List<StateValue> states)
{
    var values = states.ToDictionary(state => state.Name, state => state.Value);
    if (values.Count != 5 || !Equals(values["Brightness"], 0) ||
        !Equals(values["CoverMoving"], false) || !Equals(values["CalibratorChanging"], false) ||
        values["CoverState"].ToString() != "Closed" || values["CalibratorState"].ToString() != "Off")
        throw new InvalidOperationException("Unexpected private panel state: " +
            JsonSerializer.Serialize(states.Select(state => new { state.Name, state.Value,
                type = state.Value?.GetType().FullName })));
}

static async Task<object> WaitForSample(string folder)
{
    using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(5));
    var timer = Stopwatch.StartNew();
    JsonElement? initial = null;
    while (true) {
        using var document = JsonDocument.Parse(await SourceSnapshot(folder, deadline.Token));
        var state = document.RootElement;
        initial ??= state.Clone();
        if (!state.GetProperty("transportConnected").GetBoolean() ||
            state.GetProperty("error").ValueKind != JsonValueKind.Null ||
            state.GetProperty("generation").GetGuid() != initial.Value.GetProperty("generation").GetGuid())
            throw new InvalidOperationException("Private source is not healthy: " + state.GetRawText());
        if (state.GetProperty("sequence").GetUInt64() > 0) {
            foreach (var key in new[] { "brightness", "covermoving", "calibratorchanging", "coverstate", "calibratorstate" })
                if (!state.GetProperty("values").TryGetProperty(key, out _) ||
                    state.GetProperty("sampleErrors").TryGetProperty(key, out _))
                    throw new InvalidOperationException("Initial poll omitted " + key + ": " + state.GetRawText());
            return new { elapsedMs = timer.Elapsed.TotalMilliseconds, initial, ready = state.Clone() };
        }
        await Task.Delay(10, deadline.Token);
    }
}

static async Task<string> SourceSnapshot(string folder, CancellationToken cancellation = default)
{
    using var config = JsonDocument.Parse(File.ReadAllText(Path.Combine(folder, "hub.json")));
    using var attachment = JsonDocument.Parse(File.ReadAllText(Path.Combine(folder, "attachment.json")));
    var address = attachment.RootElement.GetProperty("address").GetString()!;
    const string prefix = @"\\.\pipe\";
    if (!address.StartsWith(prefix)) throw new InvalidOperationException("Not a named pipe");
    using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
    deadline.CancelAfter(TimeSpan.FromSeconds(5));
    using var pipe = new System.IO.Pipes.NamedPipeClientStream(".", address[prefix.Length..],
        System.IO.Pipes.PipeDirection.InOut, System.IO.Pipes.PipeOptions.Asynchronous);
    await pipe.ConnectAsync(deadline.Token);
    async Task<JsonElement> Request(int id, object command) {
        var bytes = JsonSerializer.SerializeToUtf8Bytes(new { version = 1, id, command });
        await pipe.WriteAsync(BitConverter.GetBytes(bytes.Length), deadline.Token);
        await pipe.WriteAsync(bytes, deadline.Token);
        var header = new byte[4]; await pipe.ReadExactlyAsync(header, deadline.Token);
        var length = BitConverter.ToInt32(header);
        if (length <= 0 || length > 1024 * 1024) throw new InvalidOperationException("Invalid diagnostic frame");
        var body = new byte[length]; await pipe.ReadExactlyAsync(body, deadline.Token);
        using var reply = JsonDocument.Parse(body);
        return reply.RootElement.GetProperty("result").Clone();
    }
    var hello = await Request(1, new { op = "hello" });
    if (hello.GetProperty("instanceId").GetGuid() != config.RootElement.GetProperty("instanceId").GetGuid())
        throw new InvalidOperationException("Wrong private host");
    return (await Request(2, new { op = "sourceStatus",
        source = config.RootElement.GetProperty("sources")[0].GetProperty("id").GetGuid() })).GetRawText();
}
