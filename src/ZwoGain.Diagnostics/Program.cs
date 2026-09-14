using System.Diagnostics;
using System.Text.Json;
using ZwoGain.Core;

string root = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../"));
string Option(string name, string fallback) { int i = Array.IndexOf(args, name); return i >= 0 && i + 1 < args.Length ? args[i + 1] : fallback; }
bool simulate = args.Contains("--simulate");
bool direct = args.Contains("--direct");
HostClient? currentHost = null;
HostClient Host() => currentHost = new(Option("--host", Path.Combine(root, direct ? "target/debug/zwogain-direct.exe" : "target/debug/zwogain-host.exe")), Option("--sdk", Path.Combine(root, "vendor/zwo/ASICamera2.dll")), simulate, Console.Error.WriteLine, direct);
using var cancel = new CancellationTokenSource();
Console.CancelKeyPress += (_, e) => { e.Cancel = true; cancel.Cancel(); };
using var discovery = Host();
var list = await discovery.CallAsync("list", null, TimeSpan.FromSeconds(15), cancel.Token);
Console.WriteLine(JsonSerializer.Serialize(list.Result, new JsonSerializerOptions { WriteIndented = true }));
if (!args.Contains("--capture")) return;
string? selected = Option("--camera", "");
var candidates = list.Result.EnumerateArray().Select(CameraDescriptor.Parse).Where(c => selected == "" || c.Name == selected).ToArray();
if (candidates.Length != 1) throw new InvalidOperationException("Select one unique camera using --camera");
HostClient SdkFallback() => currentHost = new(Path.Combine(root,"target/debug/zwogain-host.exe"),
    Option("--sdk",Path.Combine(root,"vendor/zwo/ASICamera2.dll")),simulate,Console.Error.WriteLine);
using var session = new CameraSession(candidates[0], Host, sdkFallbackFactory: direct && args.Contains("--sdk-fallback") ? SdkFallback : null);
session.Diagnostic += s => Console.Error.WriteLine($"{DateTime.UtcNow:O} {s}");
bool killed = false;
session.Diagnostic += s => { if (args.Contains("--kill-once") && !killed && s == "Downloading") { killed = true; Console.Error.WriteLine("TEST: terminating SDK host before download"); Process.GetProcessById(currentHost!.ProcessId).Kill(); } };
await session.ConnectAsync(cancel.Token);
Console.WriteLine(JsonSerializer.Serialize(new { session.Serial, session.SdkVersion, session.Controls }));
foreach (var (option,control) in new[] { ("--gain",0), ("--offset",5), ("--cool-target",16), ("--cooler",17), ("--dew",21) })
    if (args.Contains(option)) session.Set(control,long.Parse(Option(option,"0")));
await session.RefreshAsync(cancel.Token);
int frames = int.Parse(Option("--frames", "3"));
int bin = int.Parse(Option("--bin", "1"));
long micros = checked((long)(double.Parse(Option("--seconds", "0.05"), System.Globalization.CultureInfo.InvariantCulture) * 1e6));
int width = int.Parse(Option("--width", (candidates[0].Width / bin).ToString())), height = int.Parse(Option("--height", (candidates[0].Height / bin).ToString()));
width -= width % 8;
height -= height % 2;
int x = int.Parse(Option("--x", "0")), y = int.Parse(Option("--y", "0"));
for (int i = 0; i < frames; i++)
{
    var clock = Stopwatch.StartNew();
    var frame = await session.CaptureAsync(new(width, height, bin, x, y, micros, false), cancel.Token);
    await session.RefreshAsync(cancel.Token);
    Console.WriteLine(JsonSerializer.Serialize(new
    {
        backend = session.Backend, fallback = session.UsingSdkFallback, temperature = session.Value(8) / 10.0, coolerPower = session.Value(15),
        frame = i + 1,
        frame.Width,
        frame.Height,
        pixels = frame.Pixels.Length,
        min = frame.Pixels.Min(),
        max = frame.Pixels.Max(),
        frame.Recoveries,
        elapsedMs = clock.Elapsed.TotalMilliseconds
    }));
}

// Explicitly restore test environment controls on successful completion.
if (args.Contains("--cooler")) { session.Set(17,0); await session.RefreshAsync(cancel.Token); }
if (args.Contains("--dew")) { session.Set(21,0); await session.RefreshAsync(cancel.Token); }
