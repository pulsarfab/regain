using Regain.Core;
using Xunit;
namespace Regain.Tests;

public sealed class MountRecoveryMonitorTests
{
    private sealed class Clock : TimeProvider
    {
        private long ticks;
        public override long TimestampFrequency => TimeSpan.TicksPerSecond;
        public override long GetTimestamp() => ticks;
        public void Advance(double seconds) => ticks += (long)(seconds * TimeSpan.TicksPerSecond);
    }
    private static readonly MountObservation Good = new(true, true, false, 2.36, 62.48);
    [Fact] public void ConnectedMountWithTrackingOffLatchesAfterDebounceAndDoesNotForgetPointingLoss()
    {
        var clock = new Clock(); var monitor = new MountRecoveryMonitor(clock);
        Assert.True(monitor.Observe(Good));
        monitor.Observe(Good with { Tracking = false }); clock.Advance(4.9);
        monitor.Observe(Good with { Tracking = false }); Assert.Null(monitor.Fault);
        clock.Advance(.1); monitor.Observe(Good with { Tracking = false });
        Assert.Contains("Tracking stopped", monitor.Fault);
        monitor.Observe(Good); monitor.ResetPositionBaseline(); Assert.NotNull(monitor.Fault);
        monitor.Intent(true); Assert.Null(monitor.Fault);
    }
    [Fact] public void IntentionalStopAndCommandedMotionDoNotRequestRecovery()
    {
        var clock = new Clock(); var monitor = new MountRecoveryMonitor(clock);
        monitor.Observe(Good); monitor.Intent(false);
        monitor.Observe(Good with { Tracking = false }); clock.Advance(30);
        monitor.Observe(Good with { Tracking = false }); Assert.Null(monitor.Fault);
        monitor.Intent(true); monitor.Observe(Good with { Tracking = false }, commandedMotion: true);
        clock.Advance(30); monitor.Observe(Good with { Tracking = false }, commandedMotion: true); Assert.Null(monitor.Fault);
    }
    [Fact] public void PoleResetWithTrackingStillOnIsDetectedWithoutTreatingRaWrapAsAJump()
    {
        var clock = new Clock(); var monitor = new MountRecoveryMonitor(clock);
        monitor.Observe(Good with { RightAscensionHours = 23.999 });
        Assert.True(monitor.Observe(Good with { RightAscensionHours = .001 }));
        var reset = Good with { DeclinationDegrees = 90 };
        monitor.Observe(reset); clock.Advance(5); monitor.Observe(reset);
        Assert.Contains("position jumped", monitor.Fault);
    }
    [Fact] public void ShortTrackingGlitchDoesNotTriggerAndUnknownInitialStateDoesNotGrantMotionIntent()
    {
        var clock = new Clock(); var monitor = new MountRecoveryMonitor(clock);
        monitor.Observe(Good with { Connected = false, Tracking = false }); clock.Advance(20);
        monitor.Observe(Good with { Connected = false, Tracking = false }); Assert.Null(monitor.Fault);
        monitor.Observe(Good); monitor.Observe(Good with { Tracking = false }); clock.Advance(4);
        monitor.Observe(Good); clock.Advance(20); monitor.Observe(Good); Assert.Null(monitor.Fault);
    }
    [Fact] public void DisconnectedMountCannotSuppressDetectionWithStaleSlewingState()
    {
        var clock = new Clock(); var monitor = new MountRecoveryMonitor(clock);
        monitor.Observe(Good);
        var stale = Good with { Connected = false, Slewing = true };
        monitor.Observe(stale, commandedMotion: true); clock.Advance(5);
        monitor.Observe(stale, commandedMotion: true); Assert.Equal("Mount disconnected", monitor.Fault);
    }
    [Theory] [InlineData(0,180,5)] [InlineData(5,0,5)] [InlineData(5,180,0)] [InlineData(double.NaN,180,5)]
    public void InvalidConfigurationCannotAuthorizeRecovery(double loss, double timeout, double jump) =>
        Assert.Throws<ArgumentOutOfRangeException>(() => new MountRecoveryOptions { LostTrackingSeconds = loss, MotionTimeoutSeconds = timeout, CoordinateJumpDegrees = jump }.Validate());
}
