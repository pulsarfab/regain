using Moq;
using NINA.Core.Model.Equipment;
using NINA.Core.Utility;
using NINA.Profile.Interfaces;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class AccessoryTests : IDisposable
{
    private readonly string directory = Path.Combine(Path.GetTempPath(), "Regain-accessory-test-" + Guid.NewGuid().ToString("N"));
    private readonly string? oldEta = Environment.GetEnvironmentVariable("REGAIN_ETA_WORKER");
    private readonly string? oldFc3 = Environment.GetEnvironmentVariable("REGAIN_FC3_WORKER");
    private readonly string? oldWorker = Environment.GetEnvironmentVariable("REGAIN_ACCESSORY_WORKER");
    private readonly string? oldProfiles = Environment.GetEnvironmentVariable("REGAIN_ACCESSORY_SETTINGS");
    private readonly string? oldSimulate = Environment.GetEnvironmentVariable("REGAIN_ACCESSORY_SIMULATE");
    public AccessoryTests()
    {
        var root = new DirectoryInfo(AppContext.BaseDirectory);
        while (root is not null && !File.Exists(Path.Combine(root.FullName, "Cargo.toml"))) root = root.Parent;
        var worker = Path.Combine(root!.FullName, "target", "debug", "regain-accessories.exe");
        Assert.True(File.Exists(worker), "Build the Rust workspace before integration tests");
        Environment.SetEnvironmentVariable("REGAIN_ACCESSORY_WORKER", worker);
        Environment.SetEnvironmentVariable("REGAIN_FC3_WORKER", Path.Combine(root.FullName,"target","debug","regain-fc3.exe"));
        Environment.SetEnvironmentVariable("REGAIN_ETA_WORKER", Path.Combine(root.FullName,"target","debug","regain-eta.exe"));
        Environment.SetEnvironmentVariable("REGAIN_ACCESSORY_SETTINGS", directory);
        Environment.SetEnvironmentVariable("REGAIN_ACCESSORY_SIMULATE", "1");
    }
    [Fact]
    public async Task NativeNinaFocuserMovesAndCancellationDoesNotStartAnotherMove()
    {
        using var focuser = new EafFocuser();
        Assert.True(await focuser.Connect(CancellationToken.None));
        Assert.DoesNotContain("Regain.Calibrate", focuser.SupportedActions);
        Assert.Throws<NotSupportedException>(() => focuser.Action("Regain.Calibrate", ""));
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
        Assert.Contains("Regain.Calibrate", wheel.SupportedActions);
        Assert.Equal("null", wheel.Action("Regain.Calibrate", ""));
        Assert.Equal(-1, wheel.Position);
        Assert.Throws<InvalidOperationException>(() => wheel.Action("Regain.Calibrate", ""));
        Assert.Throws<InvalidOperationException>(() => wheel.Position = 1);
        var deadline = DateTime.UtcNow.AddSeconds(10);
        while (wheel.Position == -1) { Assert.True(DateTime.UtcNow < deadline); await Task.Delay(100); }
        Assert.Equal(0, wheel.Position); Assert.Equal(names, wheel.Names);
        Assert.Equal("Existing L", wheel.Filters[0].Name);
        wheel.Position = 0; wheel.Disconnect(); Assert.False(wheel.Connected);
        });
    }
    [Fact]
    public async Task FocusCubeMovesCancelsAndPreservesItsOwnProfile()
    {
        var hardwareSerial=Environment.GetEnvironmentVariable("REGAIN_TEST_FC3_SERIAL");
        if(!string.IsNullOrEmpty(hardwareSerial)) {
            Environment.SetEnvironmentVariable("REGAIN_ACCESSORY_SIMULATE", null);
            Directory.CreateDirectory(directory);
            File.WriteAllText(Path.Combine(directory,"fc3-nina.json"),System.Text.Json.JsonSerializer.Serialize(new {Serial=hardwareSerial}));
        }
        using var focuser = new FocusCubeFocuser();
        Assert.True(await focuser.Connect(CancellationToken.None));
        Assert.Equal("ZwoGain.FC3", focuser.Id); Assert.Equal(1000000, focuser.MaxStep);
        Assert.Equal(focuser.Action("Regain.Identity", ""), focuser.Action("ZwoGain.Identity", ""));
        int start=focuser.Position;
        try {
        await focuser.Move(start+20,CancellationToken.None,0);
        Assert.Equal(start+20,focuser.Position);
        using var cancellation=new CancellationTokenSource(100);
        await Assert.ThrowsAnyAsync<OperationCanceledException>(()=>focuser.Move(start,cancellation.Token,0));
        Assert.False(focuser.IsMoving);
        await focuser.Move(start,CancellationToken.None,0);
        } finally { focuser.Halt(); await focuser.Move(start,CancellationToken.None,0); }
        focuser.Disconnect(); Assert.False(focuser.Connected);
        Assert.Contains(hardwareSerial ?? "00:00:00:00:00:03",File.ReadAllText(Path.Combine(directory,"fc3-nina.json")));
    }
    [Fact]
    public async Task EtaPreservesTiltAndCancelsOnlyUnstartedPoints()
    {
        using var eta = new EtaFocuser();
        Assert.True(await eta.Connect(CancellationToken.None));
        Assert.Equal(1.0, eta.StepSize); Assert.Equal(1200, eta.MaxStep);
        await eta.Move(500, CancellationToken.None, 0);
        Assert.Equal(500, eta.Position);
        using (var state = System.Text.Json.JsonDocument.Parse(eta.Action("Regain.Status", "")))
            Assert.Equal(new[] {490.0,500.0,510.0}, state.RootElement.GetProperty("points_um").EnumerateArray().Select(v=>v.GetDouble()));
        await Assert.ThrowsAsync<InvalidOperationException>(()=>eta.Move(0, CancellationToken.None, 0));
        Assert.Throws<InvalidOperationException>(eta.Halt);
        using var cancellation = new CancellationTokenSource(100);
        await Assert.ThrowsAnyAsync<OperationCanceledException>(()=>eta.Move(600,cancellation.Token,0));
        var deadline = DateTime.UtcNow.AddSeconds(5);
        while (eta.IsMoving) { Assert.True(DateTime.UtcNow < deadline); await Task.Delay(100); }
        // Only point 1 started before cancellation; the other two retain their tilt positions.
        using var stopped = System.Text.Json.JsonDocument.Parse(eta.Action("Regain.Status", ""));
        Assert.Equal(new[] {590.0,500.0,510.0}, stopped.RootElement.GetProperty("points_um").EnumerateArray().Select(v=>v.GetDouble()));
        eta.Disconnect(); Assert.False(eta.Connected);
    }
    public void Dispose()
    {
        Environment.SetEnvironmentVariable("REGAIN_ACCESSORY_WORKER", oldWorker);
        Environment.SetEnvironmentVariable("REGAIN_FC3_WORKER", oldFc3);
        Environment.SetEnvironmentVariable("REGAIN_ETA_WORKER", oldEta);
        Environment.SetEnvironmentVariable("REGAIN_ACCESSORY_SETTINGS", oldProfiles);
        Environment.SetEnvironmentVariable("REGAIN_ACCESSORY_SIMULATE", oldSimulate);
        // Only this test's uniquely named temporary directory is removed.
        if (Directory.Exists(directory)) Directory.Delete(directory, true);
    }
}
