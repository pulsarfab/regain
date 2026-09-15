using System.Collections.Concurrent;
using System.Diagnostics;
using ZwoGain.Core;
using Xunit;

namespace ZwoGain.Tests;

public class RustSupervisorTests
{
    private static readonly string Worker = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../target/debug/zwogain-alpaca.exe"));
    private static readonly CameraDescriptor Camera = new("ZWO ASI676MC", 3552, 3552, true, 0, 2, 12, false, false, [1]);
    private static readonly Exposure Request = new(64, 64, 1, 0, 0, 300000, true);
    private static readonly RecoveryOptions Fast = new() { ReconnectDelaySeconds = .03, MaxRetries = 1 };

    [Fact]
    public async Task RustSupervisorRestartsWorkerAndReturnsActualRecoveryMetadata()
    {
        HostClient? host = null;
        var log = new ConcurrentQueue<string>();
        using var session = new CameraSession(Camera, () => host = new(Worker, "unused", simulate: true, direct: true, supervised: true, log: log.Enqueue), Fast);
        await session.ConnectAsync(default);
        int child = (await host!.CallAsync("diagnostics", null, TimeSpan.FromSeconds(3), default)).Result.GetProperty("processId").GetInt32();
        bool killed = false;
        session.Diagnostic += phase => { if (phase == "Exposing" && !killed) { killed = true; Process.GetProcessById(child).Kill(); } };
        session.Set(0, 200); session.Set(5, 20);
        var frame = await session.CaptureAsync(Request, default);
        Assert.True(killed);
        Assert.Equal(1, frame.Recoveries);
        Assert.Equal(4096, frame.Pixels.Length);
        Assert.Equal(200, frame.Controls[0]);
        Assert.True(host.IsAlive);
        Assert.Contains(log, m => m.Contains("capture.retry"));
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
        int child = (await host!.CallAsync("diagnostics", null, TimeSpan.FromSeconds(3), default)).Result.GetProperty("processId").GetInt32();
        int starts = 0;
        session.Diagnostic += phase => { if (phase == "Exposing" && Interlocked.Increment(ref starts) == 1) Process.GetProcessById(child).Kill(); };
        await Assert.ThrowsAsync<IOException>(() => session.CaptureAsync(Request, default));
        Assert.Equal(1, starts);
        Assert.True(host.IsAlive);
    }
}
