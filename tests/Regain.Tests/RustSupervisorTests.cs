using System.Collections.Concurrent;
using System.Diagnostics;
using Regain.Core;
using Xunit;

namespace Regain.Tests;

public class RustSupervisorTests
{
    private static readonly string Worker = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../target/debug/regain-alpaca.exe"));
    private static readonly CameraDescriptor Camera = new("ZWO ASI676MC", 3552, 3552, true, 0, 2, 12, false, false, [1]);
    private static readonly Exposure Request = new(64, 64, 1, 0, 0, 300000, true);
    private static readonly RecoveryOptions Fast = new() { ReconnectDelaySeconds = .03, MaxRetries = 1 };

    [Theory]
    [InlineData(0)]
    [InlineData(1)]
    public async Task RustSupervisorRestartsWorkerAndReturnsActualRecoveryMetadata(int usbThreshold)
    {
        HostClient? host = null;
        var log = new ConcurrentQueue<string>();
        using var session = new CameraSession(Camera, () => host = new(Worker, "unused", simulate: true, direct: true, supervised: true, log: log.Enqueue), Fast with { UsbResetAfterFailures = usbThreshold });
        await session.ConnectAsync(default);
        int child = (await host!.CallAsync("diagnostics", null, TimeSpan.FromSeconds(15), default)).Result.GetProperty("processId").GetInt32();
        // Fail all retained reads on this worker. Faults end with the worker,
        // so the replacement succeeds without depending on polling timing.
        await host.CallAsync("simulate-read-failures", new { count = 3 }, TimeSpan.FromSeconds(15), default);
        session.Set(0, 200); session.Set(5, 20);
        var frame = await session.CaptureAsync(Request, default);
        int replacement = (await host.CallAsync("diagnostics", null, TimeSpan.FromSeconds(15), default)).Result.GetProperty("processId").GetInt32();
        Assert.NotEqual(child, replacement);
        Assert.Equal(1, frame.Recoveries);
        Assert.Equal(1, session.RetryStatus.Recaptures);
        Assert.Equal(2, session.RetryStatus.UsbReads);
        Assert.Contains("state: Idle; retries: 3", session.RecoveryInfo);
        Assert.Contains("retry budget", session.RetryStatus.LastFailure);
        Assert.Equal(4096, frame.Pixels.Length);
        Assert.Equal(200, frame.Controls[0]);
        Assert.True(host.IsAlive);
        Assert.Contains(log, m => m.Contains("capture.retry"));
        Assert.Contains(log, m => m.Contains("capture.recovered") && m.Contains("2 retained-frame retries across all attempts"));
        Assert.Equal(usbThreshold, log.Count(m => m.Contains("\"event\":\"usb.reset\"")));
        var lastFailure = session.RetryStatus.LastFailure;
        await session.CaptureAsync(Request, default);
        Assert.Equal(0, session.RetryStatus.Count);
        Assert.Equal(lastFailure, session.RetryStatus.LastFailure);
    }
    [Fact]
    public async Task AbortKeepsSupervisorAliveAndRecoversForNextExposure()
    {
        HostClient? host = null;
        using var session = new CameraSession(Camera, () => host = new(Worker, "unused", simulate: true, direct: true, supervised: true), Fast);
        await session.ConnectAsync(default);
        int supervisor = host!.ProcessId;
        using var abort = new CancellationTokenSource(100);
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => session.CaptureAsync(Request with { microseconds = 2000000 }, abort.Token));
        Assert.True(host.IsAlive);
        Assert.Equal(supervisor, host.ProcessId);
        Assert.Equal(4096, (await session.CaptureAsync(Request, default)).Pixels.Length);
    }
    [Fact]
    public async Task LongExposureFailureIsNotRetriedByDotNetOutsideRustBudget()
    {
        HostClient? host = null;
        using var session = new CameraSession(Camera, () => host = new(Worker, "unused", simulate: true, direct: true, supervised: true), Fast with { MaximumRetryExposureSeconds = .1, MaxRetries = 3 });
        await session.ConnectAsync(default);
        await host!.CallAsync("simulate-read-failures", new { count = 3 }, TimeSpan.FromSeconds(15), default);
        await Assert.ThrowsAsync<IOException>(() => session.CaptureAsync(Request, default));
        Assert.Contains("state: Error; retries: 2", session.RecoveryInfo);
        Assert.Equal(0, session.RetryStatus.Recaptures);
        Assert.Contains("retry budget", session.RetryStatus.LastFailure);
        Assert.True(host.IsAlive);
    }
}
