using System.Diagnostics;
using ZwoGain.Core;
using Xunit;

namespace ZwoGain.Tests;

public class DirectBackendTests
{
    private static readonly CameraDescriptor Camera = new("ZWO ASI676MC", 3552, 3552, true, 0, 2, 12, false, false, [1, 2, 4]);
    private static readonly Exposure Exposure = new(64, 64, 1, 0, 0, 10000, true);
    private static readonly RecoveryOptions Fast = new() { MaxRetries = 1, ReconnectDelaySeconds = .05 };
    private static HostClient Host() => new(Path.GetFullPath(Path.Combine(AppContext.BaseDirectory,
        "../../../../../target/debug/zwogain-direct.exe")), "missing-sdk.dll", simulate: true, direct: true);

    [Fact]
    public async Task DirectProtocolReportsOnlyImplementedCapabilitiesAndRejectsInvalidRoiWithoutRetry()
    {
        int starts = 0;
        using var session = new CameraSession(Camera, () => { starts++; return Host(); }, Fast);
        await session.ConnectAsync(default);
        Assert.Equal("direct", session.Backend);
        Assert.Equal(new[] { 1 }, session.Camera.Bins);
        Assert.Equal(30000000, session.Controls[1].Max);
        Assert.False(session.Controls[6].Writable);
        Assert.False(session.Controls.ContainsKey(8));
        var invalid = await Assert.ThrowsAsync<SdkException>(() => session.CaptureAsync(Exposure with { width = 8 }, default));
        Assert.False(invalid.Retryable);
        Assert.Equal(1, starts);
        await Assert.ThrowsAsync<ArgumentOutOfRangeException>(() => session.CaptureAsync(Exposure with { bin = 2 }, default));
    }

    [Fact]
    public async Task DirectWorkerCrashRecoversWithSameBackendIdentityAndControls()
    {
        HostClient? current = null;
        int starts = 0;
        using var session = new CameraSession(Camera, () => { starts++; return current = Host(); }, Fast);
        bool killed = false;
        session.Diagnostic += phase => {
            if (phase == "Downloading" && !killed) { killed = true; Process.GetProcessById(current!.ProcessId).Kill(); }
        };
        await session.ConnectAsync(default);
        var identity = session.Serial;
        session.Set(0, 123); session.Set(5, 17);
        var frame = await session.CaptureAsync(Exposure, default);
        Assert.Equal(2, starts);
        Assert.Equal(1, frame.Recoveries);
        Assert.Equal(identity, session.Serial);
        Assert.Equal("direct", session.Backend);
        Assert.Equal(123, frame.Controls[0]); Assert.Equal(17, frame.Controls[5]);
        Assert.Equal(4096, frame.Pixels.Length);
        Assert.Equal((ushort)4095, frame.Pixels[^1]);
    }

    [Fact]
    public async Task CancellationTerminatesActiveDirectWorkerAndNextCaptureReconnects()
    {
        int starts = 0;
        using var session = new CameraSession(Camera, () => { starts++; return Host(); }, Fast);
        await session.ConnectAsync(default);
        using var cancel = new CancellationTokenSource(100);
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => session.CaptureAsync(Exposure with { microseconds = 2000000 }, cancel.Token));
        Assert.Equal("Aborted", session.Phase);
        var frame = await session.CaptureAsync(Exposure, default);
        Assert.Equal(2, starts);
        Assert.Equal(4096, frame.Pixels.Length);
    }

    [Fact]
    public async Task UnsupportedCameraNeverEntersDirectAcquisition()
    {
        using var host = Host();
        var error = await Assert.ThrowsAsync<SdkException>(() => host.CallAsync("open", new { name = "ZWO ASI2600MM Duo" }, TimeSpan.FromSeconds(5), default));
        Assert.False(error.Retryable);
        Assert.Contains("ASI676MC only", error.Message);
    }
}
