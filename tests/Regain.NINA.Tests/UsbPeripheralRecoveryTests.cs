using Regain.NINA;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class UsbPeripheralRecoveryTests
{
    private sealed class Clock : TimeProvider
    {
        private long ticks;
        public override long TimestampFrequency => TimeSpan.TicksPerSecond;
        public override long GetTimestamp() => Interlocked.Read(ref ticks);
        internal void Advance(double seconds) => Interlocked.Add(ref ticks, TimeSpan.FromSeconds(seconds).Ticks);
    }
    private sealed class Peripheral(string kind, bool connected = true, string? state = "position=3") : IUsbPeripheral
    {
        public string Kind => kind;
        public bool RequiresImagingState { get; set; } = true;
        internal PeripheralState Current { get; set; } = new(connected, kind + "-id", state);
        public event Action? StateChanged;
        internal int Connections, Disposals;
        internal Func<CancellationToken, Task<bool>>? Connect;
        public PeripheralState Snapshot() => Current;
        internal void Report(bool connected, string? id = null, string? state = null)
        {
            Current = new(connected, id ?? Current.DeviceId, state ?? Current.ImagingState);
            StateChanged?.Invoke();
        }
        public Task<bool> ReconnectAsync(CancellationToken token)
        {
            Connections++;
            if (Connect is not null) return Connect(token);
            Report(true);
            return Task.FromResult(true);
        }
        public void Dispose() => Disposals++;
    }
    private static UsbPeripheralRecovery Recovery(Clock clock, params Peripheral[] devices) => new(devices,
        new() { DisconnectWindowSeconds = 1 }, _ => { }, clock);

    [Fact]
    public async Task OnlyPreviouslyConnectedDevicesDroppingWithinWindowReconnectAfterCameraReturns()
    {
        var clock = new Clock();
        var wheel = new Peripheral("wheel"); var focuser = new Peripheral("focuser", false);
        var rotator = new Peripheral("rotator"); var weather = new Peripheral("weather", false);
        using var recovery = Recovery(clock, wheel, focuser, rotator, weather);
        recovery.ResetStarting(1);
        wheel.Report(false); focuser.Report(false); weather.Report(true);
        var waiting = recovery.WaitAsync(1, CancellationToken.None);
        Assert.False(waiting.IsCompleted);
        clock.Advance(2);
        await Task.Delay(300);
        Assert.False(waiting.IsCompleted); Assert.Equal(0, wheel.Connections);
        recovery.CameraReturned(1);
        await waiting.WaitAsync(TimeSpan.FromSeconds(3));
        Assert.Equal(1, wheel.Connections);
        Assert.Equal(0, focuser.Connections); Assert.Equal(0, rotator.Connections); Assert.Equal(0, weather.Connections);
    }

    [Fact]
    public async Task LateDisconnectIsOutsideCorrelationWindow()
    {
        var clock = new Clock(); var wheel = new Peripheral("wheel");
        using var recovery = Recovery(clock, wheel);
        recovery.ResetStarting(2);
        clock.Advance(2); wheel.Report(false);
        recovery.CameraReturned(2);
        await recovery.WaitAsync(2, CancellationToken.None).WaitAsync(TimeSpan.FromSeconds(3));
        Assert.Equal(0, wheel.Connections);
    }

    [Fact]
    public async Task IndependentlyRestoredDeviceIsVerifiedWithoutDuplicateConnect()
    {
        var clock = new Clock(); var wheel = new Peripheral("wheel");
        using var recovery = Recovery(clock, wheel);
        recovery.ResetStarting(3); wheel.Report(false); wheel.Report(true);
        recovery.CameraReturned(3); clock.Advance(2);
        await recovery.WaitAsync(3, CancellationToken.None).WaitAsync(TimeSpan.FromSeconds(3));
        Assert.Equal(0, wheel.Connections);
    }

    [Theory]
    [InlineData("")]
    [InlineData("a-different-device")]
    public async Task ChangedOrUnknownSelectionBlocksReplayAndExposure(string selection)
    {
        var clock = new Clock(); var wheel = new Peripheral("wheel");
        using var recovery = Recovery(clock, wheel);
        recovery.ResetStarting(4); wheel.Report(false, id: selection);
        recovery.CameraReturned(4); clock.Advance(2);
        var error = await Assert.ThrowsAsync<IOException>(() => recovery.WaitAsync(4, CancellationToken.None));
        Assert.Contains("selection", error.Message); Assert.Equal(0, wheel.Connections);
    }

    [Fact]
    public async Task ChangedOpticalStateBlocksExposureWithoutAttemptingMotion()
    {
        var clock = new Clock(); var wheel = new Peripheral("wheel");
        wheel.Connect = _ => { wheel.Report(true, state: "position=0"); return Task.FromResult(true); };
        using var recovery = Recovery(clock, wheel);
        recovery.ResetStarting(5); wheel.Report(false); recovery.CameraReturned(5); clock.Advance(2);
        var error = await Assert.ThrowsAsync<IOException>(() => recovery.WaitAsync(5, CancellationToken.None));
        Assert.Contains("optical state", error.Message); Assert.Equal(1, wheel.Connections);
    }

    [Fact]
    public async Task UnknownRequiredBaselineCannotBeReplacedWithArbitraryOpticalState()
    {
        var clock = new Clock(); var wheel = new Peripheral("wheel", state: null);
        using var recovery = Recovery(clock, wheel);
        recovery.ResetStarting(14); wheel.Report(false); recovery.CameraReturned(14); clock.Advance(2);
        var error = await Assert.ThrowsAsync<IOException>(() => recovery.WaitAsync(14, CancellationToken.None));
        Assert.Contains("optical state was unavailable", error.Message); Assert.Equal(0, wheel.Connections);
    }

    [Fact]
    public async Task DisconnectAfterConstructionUsesLatestConnectedBaseline()
    {
        var clock = new Clock(); var wheel = new Peripheral("wheel", false);
        using var recovery = Recovery(clock, wheel);
        wheel.Report(true); recovery.ResetStarting(6); wheel.Report(false);
        recovery.CameraReturned(6); clock.Advance(2);
        await recovery.WaitAsync(6, CancellationToken.None);
        Assert.Equal(1, wheel.Connections);
    }

    [Fact]
    public async Task DisconnectReportBeforeStderrStillUsesAuthoritativeResetTimeAndPriorState()
    {
        var clock = new Clock(); var wheel = new Peripheral("wheel");
        using var recovery = Recovery(clock, wheel);
        long emitted = (clock.GetUtcNow().Ticks - DateTimeOffset.UnixEpoch.Ticks) / 10;
        wheel.Report(false); // NINA's notification wins the asynchronous stderr race.
        recovery.ResetStarting(12, emitted); recovery.CameraReturned(12); clock.Advance(2);
        await recovery.WaitAsync(12, CancellationToken.None);
        Assert.Equal(1, wheel.Connections);
    }

    [Fact]
    public async Task IndependentlyRestoredChangedStateBeforeStderrStillBlocksExposure()
    {
        var clock = new Clock(); var wheel = new Peripheral("wheel");
        using var recovery = Recovery(clock, wheel);
        long emitted = (clock.GetUtcNow().Ticks - DateTimeOffset.UnixEpoch.Ticks) / 10;
        wheel.Report(false); wheel.Report(true, state: "position=0");
        recovery.ResetStarting(13, emitted); recovery.CameraReturned(13); clock.Advance(2);
        var error = await Assert.ThrowsAsync<IOException>(() => recovery.WaitAsync(13, CancellationToken.None));
        Assert.Contains("optical state", error.Message); Assert.Equal(0, wheel.Connections);
    }

    [Fact]
    public async Task MissingOrStaleResetAnchorCannotAuthorizeAnotherEpisode()
    {
        var clock = new Clock(); var wheel = new Peripheral("wheel");
        using var recovery = Recovery(clock, wheel);
        recovery.ResetStarting(7); recovery.CameraReturned(7); clock.Advance(2);
        await recovery.WaitAsync(7, CancellationToken.None);
        var next = recovery.WaitAsync(8, CancellationToken.None);
        Assert.False(next.IsCompleted);
        recovery.ResetStarting(8); wheel.Report(false);
        recovery.CameraReturned(7); clock.Advance(2);
        await Task.Delay(300); Assert.False(next.IsCompleted); Assert.Equal(0, wheel.Connections);
        recovery.CameraReturned(8); await next.WaitAsync(TimeSpan.FromSeconds(3));
        Assert.Equal(1, wheel.Connections);
    }

    [Fact]
    public async Task CancelledConnectionIsNotOverlappedOrRetried()
    {
        var clock = new Clock(); var wheel = new Peripheral("wheel");
        var started = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var unresolved = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        wheel.Connect = _ => { started.TrySetResult(); return unresolved.Task; };
        using var recovery = Recovery(clock, wheel);
        recovery.ResetStarting(9); wheel.Report(false); recovery.CameraReturned(9); clock.Advance(2);
        using var cancel = new CancellationTokenSource();
        var waiting = recovery.WaitAsync(9, cancel.Token);
        await started.Task.WaitAsync(TimeSpan.FromSeconds(3)); cancel.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => waiting);
        unresolved.TrySetResult(false);
        await Task.Delay(100); Assert.Equal(1, wheel.Connections);
    }

    [Fact]
    public async Task DisposalCancelsObservationAndUnhooksAllPeripherals()
    {
        var clock = new Clock(); var wheel = new Peripheral("wheel");
        var recovery = Recovery(clock, wheel);
        recovery.ResetStarting(10); wheel.Report(false);
        var waiting = recovery.WaitAsync(10, CancellationToken.None);
        recovery.Dispose(); recovery.Dispose();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => waiting);
        wheel.Report(true); Assert.Equal(0, wheel.Connections); Assert.Equal(1, wheel.Disposals);
    }

    [Fact]
    public void OptionsHaveBoundedDefaultAndDisabledRecoveryIsInert()
    {
        Assert.True(new UsbPeripheralRecoveryOptions().Enabled);
        Assert.Equal(15, new UsbPeripheralRecoveryOptions().DisconnectWindowSeconds);
        foreach (int invalid in new[] { 0, 61, int.MaxValue })
            Assert.Throws<ArgumentOutOfRangeException>(() => new UsbPeripheralRecoveryOptions { DisconnectWindowSeconds = invalid }.Validate());
        var wheel = new Peripheral("wheel");
        using var recovery = new UsbPeripheralRecovery([wheel], new() { Enabled = false }, _ => { });
        recovery.ResetStarting(11); wheel.Report(false); recovery.CameraReturned(11);
        Assert.Equal(0, wheel.Connections);
    }
}
