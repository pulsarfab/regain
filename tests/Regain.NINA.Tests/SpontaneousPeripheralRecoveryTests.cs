using Regain.NINA;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class SpontaneousPeripheralRecoveryTests
{
    private sealed class Optical(string kind) : IUsbPeripheral
    {
        public string Kind => kind;
        public bool RequiresFreshOptics { get; set; }
        public event Action? StateChanged { add { } remove { } }
        internal PeripheralState State = new(true, kind + "-id", "same-serial:slot-or-angle");
        internal Func<Task<PeripheralState?>>? Probe;
        public PeripheralState Snapshot() => State;
        public Task<bool> ReconnectAsync(CancellationToken token) => throw new Exception("Never replay a NINA connect or move from a fresh native read.");
        public Task<PeripheralState?> ProbeAsync(CancellationToken token) => Probe?.Invoke() ?? Task.FromResult<PeripheralState?>(State);
        public void Dispose() { }
    }

    [Fact]
    public async Task CameraWaitsForBothWheelAndRotatorRecoveryBeforeStartingAndVerifiesAgainBeforeReturning()
    {
        var wheel = new Optical("wheel"); var rotator = new Optical("rotator");
        var entered = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var returned = new TaskCompletionSource<PeripheralState?>(TaskCreationOptions.RunContinuationsAsynchronously);
        int reads = 0, captures = 0;
        wheel.Probe = () => { reads++; entered.TrySetResult(); return reads == 1 ? returned.Task : Task.FromResult<PeripheralState?>(wheel.State); };
        using var recovery = new UsbPeripheralRecovery([wheel, rotator], new(), _ => { });
        var capture = recovery.CaptureWithOpticsAsync(() => { captures++; return Task.FromResult("full-frame"); }, CancellationToken.None);
        await entered.Task; Assert.Equal(0, captures); Assert.False(capture.IsCompleted);
        returned.SetResult(wheel.State);
        Assert.Equal("full-frame", await capture); Assert.Equal(1, captures); Assert.Equal(2, reads);
    }

    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public async Task ChangedOrMissingOpticalStateWithholdsImageAndLatchesFailure(bool changed)
    {
        var wheel = new Optical("wheel");
        using var recovery = new UsbPeripheralRecovery([wheel], new(), _ => { });
        int captures = 0;
        var error = await Assert.ThrowsAsync<IOException>(() => recovery.CaptureWithOpticsAsync(() => {
            captures++;
            if (changed) wheel.State = wheel.State with { ImagingState = "different-slot" };
            else wheel.Probe = () => Task.FromResult<PeripheralState?>(null);
            return Task.FromResult("must-not-return");
        }, CancellationToken.None));
        Assert.Contains("withheld", error.Message);
        await Assert.ThrowsAsync<IOException>(() => recovery.CaptureWithOpticsAsync(() => { captures++; return Task.FromResult("blocked"); }, CancellationToken.None));
        Assert.Equal(1, captures);
    }

    [Fact]
    public async Task StaleConnectedFlagCannotAuthorizeCaptureWhenFunctionalReadFails()
    {
        var wheel = new Optical("wheel") { Probe = () => throw new IOException("HID removed; retries exhausted") };
        using var recovery = new UsbPeripheralRecovery([wheel], new(), _ => { });
        int captures = 0;
        Assert.True(wheel.Snapshot().Connected);
        await Assert.ThrowsAsync<IOException>(() => recovery.CaptureWithOpticsAsync(() => { captures++; return Task.FromResult(0); }, CancellationToken.None));
        Assert.Equal(0, captures);
    }

    [Fact]
    public async Task CameraResetCannotAuthorizeReplacementWithChangedOpticsEvenWithoutANinaDisconnectReport()
    {
        var wheel = new Optical("wheel");
        using var recovery = new UsbPeripheralRecovery([wheel], new() { DisconnectWindowSeconds = 1 }, _ => { });
        bool replacementAuthorized = false;
        await Assert.ThrowsAsync<IOException>(() => recovery.CaptureWithOpticsAsync(async () => {
            recovery.ResetStarting(21); wheel.State = wheel.State with { ImagingState = "changed-slot" };
            recovery.CameraReturned(21);
            await recovery.WaitAsync(21, CancellationToken.None);
            replacementAuthorized = true;
            return "must-not-reach-replacement";
        }, CancellationToken.None));
        Assert.False(replacementAuthorized);
    }

    [Fact]
    public async Task PreviouslyConnectedNativeOpticsCannotDisappearFromPreflightAfterNinaDisconnectsTheDriver()
    {
        var wheel = new Optical("wheel") { RequiresFreshOptics = true, Probe = () => Task.FromResult<PeripheralState?>(null) };
        using var recovery = new UsbPeripheralRecovery([wheel], new(), _ => { });
        bool captured = false;
        await Assert.ThrowsAsync<IOException>(() => recovery.CaptureWithOpticsAsync(() => {
            captured = true; return Task.FromResult("unsafe");
        }, CancellationToken.None));
        Assert.False(captured);
    }

    [Fact]
    public async Task SamePositionAfterDisconnectCannotAuthorizeTheOverlappingFrameButAllowsNextFreshCapture()
    {
        var wheel = new Optical("wheel") { State = new(true, "wheel-id", "same-slot", "first-connection") };
        using var recovery = new UsbPeripheralRecovery([wheel], new(), _ => { });
        await Assert.ThrowsAsync<OpticalExposureInterruptedException>(() => recovery.CaptureWithOpticsAsync(() => {
            wheel.State = wheel.State with { ConnectionEpoch = "reopened-connection" };
            return Task.FromResult("contaminated-frame");
        }, CancellationToken.None));
        Assert.Equal("fresh-frame", await recovery.CaptureWithOpticsAsync(() => Task.FromResult("fresh-frame"), CancellationToken.None));
    }

    [Fact]
    public async Task VerifiedReplacementGetsANewConnectionAnchorBeforeItStarts()
    {
        var wheel = new Optical("wheel") { State = new(true, "wheel-id", "same-slot", "first-connection") };
        using var recovery = new UsbPeripheralRecovery([wheel], new() { DisconnectWindowSeconds = 1 }, _ => { });
        Assert.Equal("new-frame", await recovery.CaptureWithOpticsAsync(async () => {
            recovery.ResetStarting(22); wheel.State = wheel.State with { ConnectionEpoch = "reopened-connection" };
            recovery.CameraReturned(22); await recovery.WaitAsync(22, CancellationToken.None);
            return "new-frame";
        }, CancellationToken.None));
    }
}
