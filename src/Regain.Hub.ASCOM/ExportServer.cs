using System.IO;
using Regain.Rotator;
using Regain.SerialServer;

namespace Regain.Hub.ASCOM;

internal static class ExportServer
{
    internal static int Run(string[] args)
    {
        try {
            string? ready = null;
            for (var i = 0; i < args.Length; i++) {
                if (args[i] is "--export" or "/Embedding" or "-Embedding") continue;
                if (args[i] == "--ready" && i + 1 < args.Length && ready is null) { ready = args[++i]; continue; }
                return 2;
            }
            var store = new HubSelectionStore(RegainPaths.EnvironmentVariable("REGAIN_HUB_BINDINGS") ?? RegainPaths.Profile("hub-frontends.json"));
            var root = Path.GetFullPath(Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "..", ".."));
            var executable = RegainPaths.EnvironmentVariable("REGAIN_HUB_HOST") ?? Path.Combine(root, "regain-alpaca.exe");
            var workers = Path.GetDirectoryName(Path.GetFullPath(executable));
            var classes = store.Load().Bindings.Select(binding => {
                var saved = binding.Copy();
                Type type = saved.DeviceType switch { "switch" => typeof(SwitchOutput), "safetymonitor" => typeof(SafetyOutput),
                    "observingconditions" => typeof(WeatherOutput), _ => throw new ArgumentException() };
                return new ServerClass(OutputIdentity.ClassId(saved), type, () => saved.DeviceType switch {
                    "switch" => new SwitchOutput(saved, executable, workers), "safetymonitor" => new SafetyOutput(saved, executable, workers),
                    "observingconditions" => new WeatherOutput(saved, executable, workers), _ => throw new ArgumentException()
                });
            }).ToArray();
            // Publishing factories/reading metadata does not launch the Rust
            // host or acquire equipment. Only a client's explicit Connect does.
            return LocalComServer.Run(classes, () => { }, _ => { }, ready);
        } catch { return 2; }
    }
}
