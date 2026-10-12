using System.Diagnostics;
using System.Text.Json;

// No UAC, USB, hardware or shared helper. The descendant deliberately inherits
// both output pipes, reproducing a consent/helper process outliving its client.
if (args.Length != 2) return 2;
string mode = args[0], directory = args[1];
if (mode == "--child") {
    Console.WriteLine("held stdout"); Console.Error.WriteLine("held stderr");
    File.WriteAllText(Path.Combine(directory, "child-ready"), Environment.ProcessId.ToString());
    while (!File.Exists(Path.Combine(directory, "release-child"))) await Task.Delay(10);
    return 0;
}
if (mode is not ("--spawn-and-exit" or "--spawn-and-hang")) return 2;
using var child = Process.Start(new ProcessStartInfo(Environment.ProcessPath!) {
    UseShellExecute = false, CreateNoWindow = true,
    ArgumentList = { "--child", directory }
})!;
File.WriteAllText(Path.Combine(directory, "processes.json"), JsonSerializer.Serialize(new {
    parent = Environment.ProcessId, child = child.Id
}));
if (mode == "--spawn-and-hang") await Task.Delay(Timeout.Infinite);
return 0;
