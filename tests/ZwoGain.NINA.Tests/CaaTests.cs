using Xunit;
using ZwoGain.Rotator;

namespace ZwoGain.NINA.Tests;

public class CaaTests
{
    [Fact]
    public void OneProviderEntryExposesOriginResetSeparatelyFromSync()
    {
        var device = Assert.Single(new CaaProvider().GetEquipment());
        Assert.Equal("ZWOgain CAA Rotator", device.Name);
        Assert.Contains("ZwoGain.CAA.ResetOrigin", device.SupportedActions);
        Assert.Contains("ZwoGain.CAA.RotateUnwrapped", device.SupportedActions);
        Assert.False(device.Connected); Assert.True(device.CanReverse);
        ((IDisposable)device).Dispose();
    }
    [Fact]
    public void ConnectReloadsSelectionSavedByAnotherSetupInstance()
    {
        string folder = Path.Combine(Path.GetTempPath(), "ZwoGain-Caa-" + Guid.NewGuid().ToString("N"));
        string path = Path.Combine(folder, "profile.json");
        try {
            using var client = new CaaSession(Path.Combine(folder, "missing-worker.exe"), path);
            using (var setup = new CaaSession("not-used", path)) setup.Select("0123456789abcdef");
            // Reaches process startup using the saved choice instead of rejecting the stale empty profile.
            Assert.Throws<System.ComponentModel.Win32Exception>(client.Connect);
            Assert.Equal("0123456789abcdef", client.Profile.Serial);
            Assert.False(client.Connected);
        } finally { if (File.Exists(path)) File.Delete(path); }
    }
    [Fact]
    public void SelectionSurvivesNewSessionAndChangingDeviceClearsOffset()
    {
        string path = Path.Combine(Path.GetTempPath(), "ZwoGain-Caa-" + Guid.NewGuid().ToString("N"), "profile.json");
        try {
            using (var session = new CaaSession("not-used", path)) {
                session.Select("0123456789abcdef");
                Assert.Throws<ArgumentException>(() => session.Select("serial with process arguments"));
            }
            using (var session = new CaaSession("not-used", path)) {
                Assert.Equal("0123456789abcdef", session.Profile.Serial);
                session.Profile.LogicalOffset = 42; session.Profile.Synced = true;
                session.Select("fedcba9876543210");
                Assert.Equal(0, session.Profile.LogicalOffset); Assert.False(session.Profile.Synced);
            }
        } finally { if (File.Exists(path)) File.Delete(path); }
    }
}
