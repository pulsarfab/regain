using Moq;
using NINA.Core.Model.Equipment;
using NINA.Core.Utility;
using NINA.Profile.Interfaces;
using Xunit;

namespace ZwoGain.NINA.Tests;

public sealed class AccessoryTests : IDisposable
{
    private readonly string directory = Path.Combine(Path.GetTempPath(), "ZwoGain-accessory-test-" + Guid.NewGuid().ToString("N"));
    private readonly string? oldWorker = Environment.GetEnvironmentVariable("ZWOGAIN_ACCESSORY_WORKER");
    private readonly string? oldProfiles = Environment.GetEnvironmentVariable("ZWOGAIN_ACCESSORY_SETTINGS");
    private readonly string? oldSimulate = Environment.GetEnvironmentVariable("ZWOGAIN_ACCESSORY_SIMULATE");
    public AccessoryTests()
    {
        var root = new DirectoryInfo(AppContext.BaseDirectory);
        while (root is not null && !File.Exists(Path.Combine(root.FullName, "Cargo.toml"))) root = root.Parent;
        var worker = Path.Combine(root!.FullName, "target", "debug", "zwogain-accessories.exe");
        Assert.True(File.Exists(worker), "Build the Rust workspace before integration tests");
        Environment.SetEnvironmentVariable("ZWOGAIN_ACCESSORY_WORKER", worker);
        Environment.SetEnvironmentVariable("ZWOGAIN_ACCESSORY_SETTINGS", directory);
        Environment.SetEnvironmentVariable("ZWOGAIN_ACCESSORY_SIMULATE", "1");
    }
    [Fact]
    public async Task NativeNinaFocuserMovesAndCancellationDoesNotStartAnotherMove()
    {
        using var focuser = new EafFocuser();
        Assert.True(await focuser.Connect(CancellationToken.None));
        Assert.DoesNotContain("ZwoGain.Calibrate", focuser.SupportedActions);
        Assert.Throws<NotSupportedException>(() => focuser.Action("ZwoGain.Calibrate", ""));
        int start = focuser.Position;
        await focuser.Move(start + 50, CancellationToken.None, 0);
        Assert.Equal(start + 50, focuser.Position);
        using var cancellation = new CancellationTokenSource(); cancellation.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => focuser.Move(start, cancellation.Token, 0));
        Assert.Equal(start + 50, focuser.Position);
        await focuser.Move(start, CancellationToken.None, 0); focuser.Halt();
        Assert.False(focuser.IsMoving); Assert.Equal(28.5, focuser.Temperature);
        focuser.Disconnect(); Assert.False(focuser.Connected);
        Assert.Contains("0102030405060709", File.ReadAllText(Path.Combine(directory, "eaf-nina.json")));
    }
    [Fact]
    public async Task NativeNinaWheelPreservesProfileFiltersAndInitializesMissingSlots()
    {
        // NINA's collection captures its application's dispatcher; this headless
        // test uses a thread without xUnit's synchronization context instead.
        await Task.Run(async () => {
        var profiles = new Mock<IProfileService>();
        var filters = new ObserveAllCollection<FilterInfo> { new FilterInfo("Existing L", 12, 0) };
        profiles.SetupGet(p => p.ActiveProfile.FilterWheelSettings.FilterWheelFilters).Returns(filters);
        using var wheel = new EfwFilterWheel(profiles.Object);
        Assert.True(await wheel.Connect(CancellationToken.None));
        Assert.Equal(7, wheel.Names.Length); Assert.Equal(7, wheel.Filters.Count);
        Assert.Equal("Existing L", wheel.Filters[0].Name);
        wheel.Position = 4;
        Assert.Equal(4, wheel.Position);
        Assert.Throws<ArgumentOutOfRangeException>(() => wheel.Position = 7);
        var names = wheel.Names.ToArray();
        Assert.Contains("ZwoGain.Calibrate", wheel.SupportedActions);
        Assert.Equal("null", wheel.Action("ZwoGain.Calibrate", ""));
        Assert.Equal(-1, wheel.Position);
        Assert.Throws<InvalidOperationException>(() => wheel.Action("ZwoGain.Calibrate", ""));
        Assert.Throws<InvalidOperationException>(() => wheel.Position = 1);
        var deadline = DateTime.UtcNow.AddSeconds(10);
        while (wheel.Position == -1) { Assert.True(DateTime.UtcNow < deadline); await Task.Delay(100); }
        Assert.Equal(0, wheel.Position); Assert.Equal(names, wheel.Names);
        Assert.Equal("Existing L", wheel.Filters[0].Name);
        wheel.Position = 0; wheel.Disconnect(); Assert.False(wheel.Connected);
        });
    }
    public void Dispose()
    {
        Environment.SetEnvironmentVariable("ZWOGAIN_ACCESSORY_WORKER", oldWorker);
        Environment.SetEnvironmentVariable("ZWOGAIN_ACCESSORY_SETTINGS", oldProfiles);
        Environment.SetEnvironmentVariable("ZWOGAIN_ACCESSORY_SIMULATE", oldSimulate);
        // Only this test's uniquely named temporary directory is removed.
        if (Directory.Exists(directory)) Directory.Delete(directory, true);
    }
}
