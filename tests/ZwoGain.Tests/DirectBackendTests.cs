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
        var error = await Assert.ThrowsAsync<SdkException>(() => host.CallAsync("open", new { name = "ZWO ASI6200MM Pro P25" }, TimeSpan.FromSeconds(5), default));
        Assert.False(error.Retryable);
        Assert.Contains("unsupported SDK-less camera model", error.Message);
    }
    private static CameraDescriptor Duo(bool guide = false) => guide
        ? new("ZWO ASI220MM Mini", 1920, 1080, false, 0, 4, 12, false, false, [1,2])
        : new("ZWO ASI2600MM Duo", 6248, 4176, false, 0, 3.76, 16, true, false, [1,2,3,4]);

    [Theory]
    [InlineData(false, 1)] [InlineData(false, 2)] [InlineData(false, 3)] [InlineData(false, 4)]
    [InlineData(true, 1)] [InlineData(true, 2)]
    public async Task DuoSensorsExposeActualCapabilitiesAndCaptureBinnedRoi(bool guide, int bin)
    {
        using var session = new CameraSession(Duo(guide), Host, Fast);
        await session.ConnectAsync(default);
        Assert.Equal(guide ? 200 : 0, session.Controls[5].Min);
        Assert.Equal(guide ? 0 : -25, session.Controls[0].Min);
        session.Set(0, guide ? 100 : -25);
        session.Set(5, guide ? 300 : 50);
        if (!guide) {
            session.Set(16, 20); session.Set(17, 1); session.Set(21, 1);
            await session.RefreshAsync(default);
            Assert.Equal(20, session.Value(16)); Assert.Equal(1, session.Value(21));
        }
        var frame = await session.CaptureAsync(Exposure with { bin = bin, x = 16, y = 2, microseconds = 100000 }, default);
        Assert.Equal(4096, frame.Pixels.Length);
        Assert.Equal(guide ? 100 : -25, frame.Controls[0]);
        Assert.Equal(bin, frame.Exposure.bin);
    }

    private static HostClient Fallback(CameraDescriptor camera, string serial = "direct-simulator") {
        var h = new HostClient(Path.GetFullPath(Path.Combine(AppContext.BaseDirectory,
            "../../../../../target/debug/zwogain-host.exe")), "unused", simulate: true);
        h.CallAsync("simulation", new { name = camera.Name, width = camera.Width, height = camera.Height,
            bins = camera.Bins, cooled = camera.Cooled, serial, instant = true, temperature = 250 },
            TimeSpan.FromSeconds(5), default).GetAwaiter().GetResult();
        return h;
    }
    [Theory]
    [InlineData(false)] [InlineData(true)]
    public async Task UnsupportedDirectExposureRoutesBeforeStartingEvenWithRetriesDisabled(bool guide)
    {
        int fallbackStarts = 0;
        using var session = new CameraSession(Duo(guide), Host, Fast with { MaxRetries = 0 },
            sdkFallbackFactory: () => { fallbackStarts++; return Fallback(Duo(guide)); });
        await session.ConnectAsync(default);
        session.Set(0, 100); session.Set(5, guide ? 300 : 50);
        var frame = await session.CaptureAsync(Exposure with { microseconds = 31000000 }, default);
        Assert.Equal(0, frame.Recoveries);
        Assert.Equal(1, fallbackStarts);
        Assert.Equal("sdk", session.Backend); Assert.True(session.UsingSdkFallback);
        Assert.Equal(100, frame.Controls[0]); Assert.Equal(guide ? 300 : 50, frame.Controls[5]);
        await session.CaptureAsync(Exposure, default);
        Assert.Equal(1, fallbackStarts);
    }
    [Theory]
    [InlineData(30, true)] [InlineData(0, false)]
    public async Task FailedDirectCaptureUsesSdkOnlyInsideSharedRetryBudget(double limit, bool expected)
    {
        HostClient? current = null;
        int fallbackStarts = 0;
        using var session = new CameraSession(Duo(), () => current = Host(), Fast with { MaximumRetryExposureSeconds = limit },
            sdkFallbackFactory: () => { fallbackStarts++; return Fallback(Duo()); });
        bool killed = false;
        session.Diagnostic += phase => { if (phase == "Downloading" && !killed) { killed = true; Process.GetProcessById(current!.ProcessId).Kill(); } };
        await session.ConnectAsync(default);
        session.Set(16, 20); session.Set(17, 1); session.Set(21, 1);
        if (expected) {
            var frame = await session.CaptureAsync(Exposure, default);
            Assert.Equal(1, frame.Recoveries); Assert.Equal("sdk", session.Backend);
            Assert.Equal(20, frame.Controls[16]); Assert.Equal(1, frame.Controls[17]); Assert.Equal(1, frame.Controls[21]);
        } else await Assert.ThrowsAsync<IOException>(() => session.CaptureAsync(Exposure, default));
        Assert.Equal(expected ? 1 : 0, fallbackStarts);
    }
    [Fact]
    public async Task FallbackCannotSubstituteAnotherPhysicalCamera()
    {
        using var session = new CameraSession(Duo(), Host, Fast,
            sdkFallbackFactory: () => Fallback(Duo(), "other-device"));
        await session.ConnectAsync(default);
        await Assert.ThrowsAnyAsync<IOException>(() => session.CaptureAsync(Exposure with { microseconds = 31000000 }, default));
        Assert.Equal("direct-simulator", session.Serial);
    }

}
