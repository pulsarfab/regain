using System.Text.Json;
using Microsoft.Win32;
using Regain.Hub.ASCOM;

namespace Regain.TestFixtures;

internal static class HubLocalRegistryFixture
{
    // All writes remain under one unique fixture key. Nothing is activated or
    // installed, and the fixture GUID is never a registered COM class.
    internal static void Run() {
        var path = @"Software\PulsarFab\Regain\Tests\LocalCatalog-" + Guid.NewGuid().ToString("N");
        try {
            using (var root = Registry.CurrentUser.CreateSubKey(path)) {
                foreach (var pair in new[] { ("Machine", "machine"), ("User", "user") }) {
                    using var driver = root.CreateSubKey(pair.Item1 + @"\Software\ASCOM\Focuser Drivers\Fixture.Focuser"); driver.SetValue("", pair.Item2);
                    using var broken = root.CreateSubKey(pair.Item1 + @"\Software\ASCOM\Focuser Drivers\Fixture.Missing"); broken.SetValue("", "missing class");
                }
                using var merged = root.CreateSubKey(@"Classes\Fixture.Focuser\CLSID"); merged.SetValue("", "22222222-2222-4222-8222-222222222222");
                using var machine = root.CreateSubKey(@"Machine\Software\Classes\Fixture.Focuser\CLSID"); machine.SetValue("", "33333333-3333-4333-8333-333333333333");
            }
            foreach (var elevated in new[] { false, true }) {
                using var registry = new HubSystemDriverRegistry(Registry.CurrentUser.OpenSubKey(path + @"\Machine")!,
                    Registry.CurrentUser.OpenSubKey(path + @"\User")!, Registry.CurrentUser.OpenSubKey(path + @"\Classes")!, elevated);
                var catalog = HubDriverCatalog.Read(registry, "focuser");
                if (catalog.Incomplete || catalog.Entries.Count != 2) throw new InvalidOperationException("Invalid private registry catalog");
                var driver = catalog.Entries.Single(e => e.ProgId == "Fixture.Focuser");
                var clsid = elevated ? "33333333-3333-4333-8333-333333333333" : "22222222-2222-4222-8222-222222222222";
                if (driver.Name != (elevated ? "machine" : "user") || driver.ClassId != Guid.Parse(clsid) ||
                    catalog.Entries.Single(e => e.ProgId == "Fixture.Missing").ClassId.HasValue)
                    throw new InvalidOperationException("Registry view or user/machine precedence changed");
                if (HubDriverCatalog.Read(registry, "camera").Entries.Count != 0) throw new InvalidOperationException("Wrong registry category");
            }
        } finally { Registry.CurrentUser.DeleteSubKeyTree(path, false); }
        Console.WriteLine("Local catalog: private registry enumeration, architecture view and elevated/user precedence passed");
    }
}
