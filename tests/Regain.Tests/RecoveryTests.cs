using System.Diagnostics;
using Regain.Core;
using Xunit;

namespace Regain.Tests;

public class RecoveryTests
{
    [Theory]
    [InlineData(5, 20, true)]
    [InlineData(5, 101, false)]
    [InlineData(16, 0, false)]
    public async Task SdkOffsetClampingPreservesAppliedMetadataAndRecoveryButOtherMismatchesFail(int control, long minimum, bool accepted)
    {
        int starts = 0;
        using var session = new CameraSession(Camera, () =>
        {
            var h = Host();
            h.CallAsync("simulation", new { clampControl = control, clampMinimum = minimum }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
            if (starts++ == 0)
                h.CallAsync("fault", new { kind = "download" }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
            return h;
        }, Fast with { MaxRetries = 1 });
        await session.ConnectAsync(default);
        session.Set(5, 0);
        if (accepted)
        {
            var frame = await session.CaptureAsync(Exposure, default);
            Assert.Equal(1, frame.Recoveries);
            Assert.Equal(20, frame.Controls[5]);
            Assert.Equal(20, session.Value(5));
            Assert.Equal(-10, frame.Controls[16]);
            var next = await session.CaptureAsync(Exposure, default);
            Assert.Equal(0, next.Recoveries);
            Assert.Equal(20, next.Controls[5]);
        }
        else
        {
            var error = await Assert.ThrowsAsync<IOException>(() => session.CaptureAsync(Exposure, default));
            Assert.Contains("read-back", error.Message);
        }
        Assert.Equal(2, starts);
    }

    [Theory]
    [InlineData(30000000L, 30, true)]
    [InlineData(30000001L, 30, false)]
    [InlineData(60000000L, 60, true)]
    [InlineData(60000001L, 60, false)]
    [InlineData(10000L, 0, false)]
    public async Task RetryDurationThresholdIsInclusiveAndConfigurable(long microseconds, double threshold, bool retries)
    {
        int starts = 0;
        using var session = new CameraSession(Camera, () =>
        {
            var h = Host();
            h.CallAsync("simulation", new
            {
                instant = true
            }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
            if (starts++ == 0)
                h.CallAsync("fault", new
                {
                    kind = "download"
                }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
            return h;
        }, Fast with
        {
            MaxRetries = 1,
            MaximumRetryExposureSeconds = threshold
        });
        await session.ConnectAsync(default);
        if (retries)
        {
            var frame = await session.CaptureAsync(Exposure with
            {
                microseconds = microseconds
            }, default);
            Assert.Equal(1, frame.Recoveries);
            Assert.Equal(microseconds, frame.Exposure.microseconds);
        }
        else
        {
            var error = await Assert.ThrowsAsync<IOException>(() => session.CaptureAsync(Exposure with { microseconds = microseconds }, default));
            Assert.Contains("after 1 attempts", error.Message);
        }
        Assert.Equal(retries ? 2 : 1, starts);
    }
    [Theory]
    [InlineData(1200000000L, 1, false)]
    [InlineData(1200000000L, 3, false)]
    [InlineData(10000L, 3, true)]
    public async Task RereadsPrecedeRecaptureAndIgnoreExposureLimit(long microseconds, int failures, bool recaptures)
    {
        int starts = 0, rereads = 0;
        HostClient? current = null;
        var options = Fast with { ReadyFrameDownloadRetries = new RecoveryOptions().ReadyFrameDownloadRetries };
        Assert.Equal(2, options.ReadyFrameDownloadRetries);
        using var session = new CameraSession(Camera, () =>
        {
            current = Host();
            current.CallAsync("simulation", new { instant = true }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
            if (starts++ == 0)
                current.CallAsync("fault", new { kind = "download" }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
            return current;
        }, options);
        session.Diagnostic += phase =>
        {
            if (phase.StartsWith("Transfer failure:") && --failures > 0)
                current!.CallAsync("fault", new { kind = "download" }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
            if (phase.StartsWith("Rereading ready frame")) rereads++;
        };
        await session.ConnectAsync(default);
        if (failures > options.ReadyFrameDownloadRetries && !recaptures)
            await Assert.ThrowsAsync<IOException>(() => session.CaptureAsync(Exposure with { microseconds = microseconds }, default));
        else
        {
            var frame = await session.CaptureAsync(Exposure with { microseconds = microseconds }, default);
            Assert.Equal(recaptures ? 1 : 0, frame.Recoveries);
            Assert.Equal(microseconds, frame.Exposure.microseconds);
            Assert.Equal((ushort)321, frame.Pixels[321]);
        }
        Assert.Equal(recaptures ? 2 : 1, starts);
        Assert.InRange(rereads, 1, options.ReadyFrameDownloadRetries);
        Assert.Equal(0, failures);
    }
    [Fact]
    public void ExistingSettingsReceiveThirtySecondDefault()
    {
        Assert.Equal(30, System.Text.Json.JsonSerializer.Deserialize<RecoveryOptions>("{\"MaxRetries\":2}")!.MaximumRetryExposureSeconds);
        Assert.Throws<ArgumentOutOfRangeException>(() => (Fast with { MaximumRetryExposureSeconds = -1 }).Validate());
        Assert.Throws<ArgumentOutOfRangeException>(() => (Fast with { MaximumRetryExposureSeconds = double.NaN }).Validate());
    }
    private static readonly string Root = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../"));
    private static HostClient Host() => new(Path.Combine(Root, "target/debug/regain-host.exe"), "unused", true);
    private static readonly CameraDescriptor Camera = new("ZWO Simulated", 960, 640, true, 0, 3.76, 16, true, false, [1, 2, 4]);
    private static readonly Exposure Exposure = new(960, 640, 1, 0, 0, 10000, false);
    private static RecoveryOptions Fast => new() { ReconnectDelaySeconds = .05, CommandTimeoutSeconds = 15, DownloadTimeoutSeconds = .2, CoolingSampleSeconds = .01, CoolingStableSamples = 2, ReadyFrameDownloadRetries = 0 };
    [Theory]
    [InlineData("download")]
    [InlineData("crash")]
    [InlineData("hang")]
    public async Task RecoversTransferFailureByReplacingHostAndRestoringControls(string fault)
    {
        int starts = 0;
        var phases = new System.Collections.Concurrent.ConcurrentQueue<string>();
        using var session = new CameraSession(Camera, () =>
        {
            var h = Host();
            if (starts++ == 0)
                h.CallAsync("fault", new
                {
                    kind = fault
                }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
            return h;
        }, Fast);
        session.Diagnostic += phases.Enqueue;
        await session.ConnectAsync(default);
        session.Set(0, 123);
        session.Set(5, 17);
        session.Set(16, -12);
        session.Set(17, 1);
        var result = await session.CaptureAsync(Exposure, default);
        Assert.Equal(1, result.Recoveries);
        Assert.Equal(2, starts);
        Assert.Equal(960 * 640, result.Pixels.Length);
        Assert.Equal((ushort)321, result.Pixels[321]);
        Assert.Equal(123, session.Value(0));
        Assert.Contains(phases, p => p.StartsWith("Restoring cooling"));
        Assert.Equal("Idle", session.Phase);
    }
    [Fact]
    public async Task ExhaustionIsBounded()
    {
        int starts = 0;
        using var session = new CameraSession(Camera, () => { starts++; var h = Host(); h.CallAsync("fault", new { kind = "download" }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult(); return h; }, Fast with
        {
            MaxRetries = 2
        });
        await session.ConnectAsync(default);
        await Assert.ThrowsAsync<IOException>(() => session.CaptureAsync(Exposure, default));
        Assert.Equal(3, starts);
    }
    [Fact]
    public async Task CancellationDuringExposureDoesNotRetryAndNextCaptureWorks()
    {
        int starts = 0;
        using var session = new CameraSession(Camera, () => { starts++; return Host(); }, Fast);
        await session.ConnectAsync(default);
        using var cancel = new CancellationTokenSource(150);
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => session.CaptureAsync(Exposure with { microseconds = 10000000 }, cancel.Token));
        Assert.Equal(1, starts);
        var result = await session.CaptureAsync(Exposure, default);
        Assert.Equal(960 * 640, result.Pixels.Length);
        Assert.Equal(2, starts);
    }
    [Fact]
    public async Task ReadyDownloadRetryKeepsSameExposureAndProcess()
    {
        int starts = 0;
        using var session = new CameraSession(Camera, () => { starts++; var h = Host(); h.CallAsync("fault", new { kind = "download" }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult(); return h; }, Fast with
        {
            ReadyFrameDownloadRetries = 1
        });
        await session.ConnectAsync(default);
        var result = await session.CaptureAsync(Exposure, default);
        Assert.Equal(0, result.Recoveries);
        Assert.Equal(1, starts);
        Assert.Equal(2, session.LastSdkExposureState);
    }
    [Fact]
    public async Task InvalidRoiFailsWithoutRetry()
    {
        int starts = 0;
        using var session = new CameraSession(Camera, () => { starts++; return Host(); }, Fast);
        await session.ConnectAsync(default);
        await Assert.ThrowsAsync<ArgumentOutOfRangeException>(() => session.CaptureAsync(Exposure with { width = 968 }, default));
        Assert.Equal(1, starts);
    }
    [Fact]
    public async Task BinaryFramesRemainExactAcrossConsecutiveCaptures()
    {
        using var session = new CameraSession(Camera, Host, Fast);
        await session.ConnectAsync(default);
        for (int j = 0; j < 5; j++)
        {
            var result = await session.CaptureAsync(Exposure with
            {
                width = 480,
                height = 320,
                bin = 2
            }, default);
            Assert.Equal((ushort)65535, result.Pixels[65535]);
            Assert.Equal((ushort)0, result.Pixels[65536]);
        }
    }
    [Fact]
    public void RejectsUnboundedConfiguration()
    {
        Assert.Throws<ArgumentOutOfRangeException>(() => (Fast with { MaxRetries = 100 }).Validate());
        Assert.Throws<ArgumentOutOfRangeException>(() => (Fast with { DownloadTimeoutSeconds = double.NaN }).Validate());
    }
    [Fact]
    public async Task WarmCameraDoesNotResumeBeforeCoolingDeadlineAndFailsBoundedly()
    {
        int starts = 0;
        var phases = new System.Collections.Concurrent.ConcurrentQueue<string>();
        using var session = new CameraSession(Camera, () =>
        {
            var h = Host();
            if (starts++ == 0)
                h.CallAsync("fault", new
                {
                    kind = "download"
                }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
            else
                h.CallAsync("simulation", new
                {
                    temperature = 100
                }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
            return h;
        }, Fast with
        {
            MaxRetries = 1,
            CoolingTimeoutSeconds = .08
        });
        session.Diagnostic += phases.Enqueue;
        await session.ConnectAsync(default);
        await Assert.ThrowsAsync<IOException>(() => session.CaptureAsync(Exposure, default));
        Assert.Equal(1, phases.Count(p => p == "Starting exposure"));
        Assert.Equal(2, starts);
    }
    [Fact]
    public async Task CoolingOffSkipsThermalWait()
    {
        int starts = 0;
        using var session = new CameraSession(Camera, () =>
        {
            var h = Host();
            if (starts++ == 0)
                h.CallAsync("fault", new
                {
                    kind = "download"
                }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
            else
                h.CallAsync("simulation", new
                {
                    temperature = 100
                }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
            return h;
        }, Fast with
        {
            MaxRetries = 1,
            CoolingTimeoutSeconds = .08
        });
        await session.ConnectAsync(default);
        session.Set(17, 0);
        Assert.Equal(1, (await session.CaptureAsync(Exposure, default)).Recoveries);
    }
    [Theory]
    [InlineData(1, false)]
    [InlineData(19, false)]
    [InlineData(20, true)]
    public async Task ColdSensorAtPriorTemperatureMustAlsoRecoverCoolerOutput(int restoredPower, bool resumes)
    {
        int starts = 0;
        var phases = new System.Collections.Concurrent.ConcurrentQueue<string>();
        using var session = new CameraSession(Camera, () =>
        {
            var h = Host();
            if (starts++ == 0)
                h.CallAsync("fault", new { kind = "download" }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
            else
                h.CallAsync("simulation", new { coolerPower = restoredPower }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
            return h;
        }, Fast with { MaxRetries = 1, CoolingTimeoutSeconds = .1 });
        session.Diagnostic += phases.Enqueue;
        await session.ConnectAsync(default);
        if (resumes) Assert.Equal(1, (await session.CaptureAsync(Exposure, default)).Recoveries);
        else await Assert.ThrowsAsync<IOException>(() => session.CaptureAsync(Exposure, default));
        Assert.Equal(resumes ? 2 : 1, phases.Count(p => p == "Starting exposure"));
        Assert.Contains(phases, p => p.Contains($"power {restoredPower}% (prior 30%)"));
    }
    [Theory]
    [InlineData(-20, true)]
    [InlineData(-10, false)]
    public async Task SettlingAcceptsFurtherCoolingTowardTargetButNotUncommandedOvercooling(int target, bool resumes)
    {
        int starts = 0;
        using var session = new CameraSession(Camera, () =>
        {
            var h = Host();
            if (starts++ == 0)
                h.CallAsync("fault", new { kind = "download" }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
            else
                h.CallAsync("simulation", new { temperature = -150 }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
            return h;
        }, Fast with { MaxRetries = 1, CoolingTimeoutSeconds = .1 });
        await session.ConnectAsync(default);
        session.Set(16, target);
        if (resumes) Assert.Equal(1, (await session.CaptureAsync(Exposure, default)).Recoveries);
        else await Assert.ThrowsAsync<IOException>(() => session.CaptureAsync(Exposure, default));
    }
    [Theory]
    [InlineData(6248, 4176)]
    [InlineData(9576, 6388)]
    public async Task LargeSensorBinaryTransport(int width, int height)
    {
        using var session = new CameraSession(Camera with
        {
            Width = width,
            Height = height
        }, () =>
        {
            var h = Host();
            h.CallAsync("simulation", new
            {
                width,
                height
            }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
            return h;
        }, Fast with
        {
            DownloadTimeoutSeconds = 10
        });
        await session.ConnectAsync(default);
        var result = await session.CaptureAsync(Exposure with
        {
            width = width,
            height = height
        }, default);
        Assert.Equal(width * height, result.Pixels.Length);
        Assert.Equal(unchecked((ushort)(width * height - 1)), result.Pixels[^1]);
    }
    [Fact]
    public async Task InvalidSdkParameterIsNotRetried()
    {
        int starts = 0;
        using var session = new CameraSession(Camera, () => { starts++; var h = Host(); h.CallAsync("fault", new { kind = "invalid" }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult(); return h; }, Fast);
        await session.ConnectAsync(default);
        var error = await Assert.ThrowsAsync<SdkException>(() => session.CaptureAsync(Exposure, default));
        Assert.Equal(8, error.Code);
        Assert.Equal(1, starts);
    }
    [Fact]
    public async Task CancellationDuringReconnectDelayDoesNotLaunchReplacement()
    {
        int starts = 0;
        using var cancel = new CancellationTokenSource();
        using var session = new CameraSession(Camera, () => { starts++; var h = Host(); h.CallAsync("fault", new { kind = "download" }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult(); return h; }, Fast with
        {
            ReconnectDelaySeconds = 10
        });
        session.Diagnostic += p => { if (p.StartsWith("Reconnect delay")) cancel.Cancel(); };
        await session.ConnectAsync(default);
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => session.CaptureAsync(Exposure, cancel.Token));
        Assert.Equal(1, starts);
    }
}
