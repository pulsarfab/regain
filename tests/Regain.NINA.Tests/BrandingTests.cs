using Regain.Rotator;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class BrandingTests
{
    [Fact]
    public void CopiesLegacyProfilesWithoutChangingEitherIdentityOrNewSettings()
    {
        string root = Path.Combine(Path.GetTempPath(), "regain-migration-" + Guid.NewGuid().ToString("N"));
        try {
            string legacy = Path.Combine(root, "ZwoGain", "Accessories", "fc3-ascom.json");
            Directory.CreateDirectory(Path.GetDirectoryName(legacy)!);
            File.WriteAllText(legacy, "{\"Serial\":\"48:27:E2:45:B1:16\"}");
            string path = RegainPaths.Profile(Path.Combine("Accessories", "fc3-ascom.json"), root);
            Assert.Equal(File.ReadAllText(legacy), File.ReadAllText(path));
            File.WriteAllText(path, "new settings");
            RegainPaths.Profile(Path.Combine("Accessories", "fc3-ascom.json"), root);
            Assert.Equal("new settings", File.ReadAllText(path));
            Assert.Contains("Serial", File.ReadAllText(legacy));
            Assert.Equal("regain.caa.status", RegainPaths.ActionName("ZwoGain.CAA.Status"));
        } finally { if (Directory.Exists(root)) Directory.Delete(root, true); }
    }
}
