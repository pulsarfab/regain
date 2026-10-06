using System.IO;
using System.Security.Principal;
using Regain.Rotator;
using Regain.SerialServer;

namespace Regain.Hub.ASCOM;

internal static class ExportServer
{
    internal static int Run(string[] args)
    {
        string? ready = null;
        void Progress(string state) { if (ready is not null) File.AppendAllText(ready + ".startup", state + Environment.NewLine); }
        try {
            string? bindings = null, host = null, owner = null;
            for (var i = 0; i < args.Length; i++) {
                if (args[i] is "--export" or "/Embedding" or "-Embedding") continue;
                if (args[i] == "--ready" && i + 1 < args.Length && ready is null) { ready = args[++i]; continue; }
                if (args[i] == "--bindings" && i + 1 < args.Length && bindings is null) { bindings = AbsoluteFile(args[++i]); continue; }
                if (args[i] == "--host" && i + 1 < args.Length && host is null) { host = AbsoluteFile(args[++i]); continue; }
                if (args[i] == "--owner-sid" && i + 1 < args.Length && owner is null) { owner = new SecurityIdentifier(args[++i]).Value; continue; }
                return 2;
            }
            using var identity = WindowsIdentity.GetCurrent();
            if (owner is not null && owner != identity.User?.Value) return 2;
            Progress("options accepted");
            var store = new HubSelectionStore(bindings ?? RegainPaths.EnvironmentVariable("REGAIN_HUB_BINDINGS") ?? RegainPaths.Profile("hub-frontends.json"));
            var root = Path.GetFullPath(Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "..", ".."));
            // An explicit binding path is a registered launch. Its host must
            // come from the registered command or this install, not inherited
            // per-user environment that could point at a different executable.
            var executable = host ?? (bindings is null ? RegainPaths.EnvironmentVariable("REGAIN_HUB_HOST") : null)
                ?? Path.Combine(root, "regain-alpaca.exe");
            var workers = Path.GetDirectoryName(Path.GetFullPath(executable));
            var classes = store.Load().Bindings.Select(binding => {
                var saved = binding.Copy();
                Type type = saved.DeviceType switch { "switch" => typeof(SwitchOutput), "safetymonitor" => typeof(SafetyOutput),
                    "observingconditions" => typeof(WeatherOutput), "focuser" => typeof(FocuserOutput), _ => throw new ArgumentException() };
                return new ServerClass(OutputIdentity.ClassId(saved), type, () => saved.DeviceType switch {
                    "switch" => new SwitchOutput(saved, executable, workers), "safetymonitor" => new SafetyOutput(saved, executable, workers),
                    "observingconditions" => new WeatherOutput(saved, executable, workers), "focuser" => new FocuserOutput(saved, executable, workers), _ => throw new ArgumentException()
                });
            }).ToArray();
            Progress("bindings loaded");
            // Publishing factories/reading metadata does not launch the Rust
            // host or acquire equipment. Only a client's explicit Connect does.
            return LocalComServer.Run(classes, () => { }, Progress, ready);
        } catch { try { Progress("export startup failed"); } catch { } return 2; }
    }
    private static string AbsoluteFile(string path)
    {
        if (!Path.IsPathRooted(path) || !string.Equals(Path.GetPathRoot(path), Path.GetPathRoot(Path.GetFullPath(path)), StringComparison.OrdinalIgnoreCase)
            || path.Any(char.IsControl) || !File.Exists(path)) throw new ArgumentException("An existing absolute file is required");
        return Path.GetFullPath(path);
    }
}
