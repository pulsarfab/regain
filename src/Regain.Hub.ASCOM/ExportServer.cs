using System.IO;
using System.Security.Cryptography;
using System.Text;
using Regain.Rotator;
using Regain.SerialServer;

namespace Regain.Hub.ASCOM;

/// Stable bound identities shared by registration and the executable's catalogue.
/// Reordering, renaming, paths and chooser metadata never change these IDs.
public static class OutputIdentity
{
    public static Guid ClassId(HubSelection binding)
    {
        if (binding.InstanceId == Guid.Empty || binding.OutputId == Guid.Empty || !HubSelection.Types.Contains(binding.DeviceType))
            throw new ArgumentException("A supported hub output identity is required");
        var space = Guid.Parse("6ba7b811-9dad-11d1-80b4-00c04fd430c8").ToByteArray();
        NetworkOrder(space);
        var name = Encoding.UTF8.GetBytes($"https://pulsarfab.com/regain/ascom-hub/output/{binding.InstanceId:D}/{binding.OutputId:D}/{binding.DeviceType}");
        using var sha1 = SHA1.Create();
        var bytes = sha1.ComputeHash(space.Concat(name).ToArray()).Take(16).ToArray();
        bytes[6] = (byte)((bytes[6] & 0x0f) | 0x50); bytes[8] = (byte)((bytes[8] & 0x3f) | 0x80);
        NetworkOrder(bytes);
        return new Guid(bytes);
    }
    private static void NetworkOrder(byte[] bytes)
    { Array.Reverse(bytes, 0, 4); Array.Reverse(bytes, 4, 2); Array.Reverse(bytes, 6, 2); }
    public static string ProgId(HubSelection binding) => "Rgn.H" + (binding.DeviceType switch {
        "switch" => "S", "safetymonitor" => "M", "observingconditions" => "W", _ => throw new ArgumentException("Unknown hub output class")
    }) + "." + ClassId(binding).ToString("N"); // 39 characters: COM's maximum.
}
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
