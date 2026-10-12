using System.Collections.Concurrent;
using System.Text.Json;
using Regain.Core;
using Xunit;

namespace Regain.Tests;

public class CameraUsbRecoveryTests
{
    private static readonly string Supervisor = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../target/debug/regain-alpaca.exe"));
    private static readonly string Worker = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../target/debug/regain-device.exe"));
    private static readonly CameraDescriptor Camera = new("ZWO ASI676MC", 3552, 3552, true, 0, 2, 12, false, false, [1]);
    private static readonly RecoveryOptions Fast = new() { ReconnectDelaySeconds = .03, MaxRetries = 1 };
    private const string Prefix = "REGAIN_DIAGNOSTIC ";
    private static string Live(int pid, string kind = "usb.reset") => Prefix + JsonSerializer.Serialize(new {
        version = 1, @event = kind, message = "live USB lifecycle", pid, supervisor = new { pid, cameraSlot = (int?)null }
    });

    [Theory]
    [InlineData("usb.reset", CameraUsbRecoveryPhase.Resetting)]
    [InlineData("usb.returned", CameraUsbRecoveryPhase.CameraReturned)]
    public void OnlyExactLiveSupervisorLifecycleRecordsAreAccepted(string kind, CameraUsbRecoveryPhase expected)
    {
        Assert.True(CameraUsbRecovery.TryParse(Live(123, kind), 123, true, out var phase));
        Assert.Equal(expected, phase.Phase);
        Assert.False(CameraUsbRecovery.TryParse(Live(123, kind), 123, false, out _));
        Assert.False(CameraUsbRecovery.TryParse(Live(123, kind), 456, true, out _));
    }

    [Theory]
    [InlineData("legacy usb.reset")]
    [InlineData("REGAIN_DIAGNOSTIC {broken")]
    [InlineData("REGAIN_DIAGNOSTIC []")]
    [InlineData("REGAIN_DIAGNOSTIC {\"version\":2,\"event\":\"usb.reset\",\"message\":\"x\",\"pid\":123,\"supervisor\":{\"pid\":123}}")]
    [InlineData("REGAIN_DIAGNOSTIC {\"version\":\"1\",\"event\":\"usb.reset\",\"message\":\"x\",\"pid\":123,\"supervisor\":{\"pid\":123}}")]
    [InlineData("REGAIN_DIAGNOSTIC {\"version\":1,\"event\":\"usb.reset\",\"message\":null,\"pid\":123,\"supervisor\":{\"pid\":123}}")]
    [InlineData("REGAIN_DIAGNOSTIC {\"version\":1,\"event\":\"usb.reset\",\"message\":\"x\",\"pid\":123}")]
    [InlineData("REGAIN_DIAGNOSTIC {\"version\":1,\"event\":\"usb.reset\",\"message\":\"x\",\"pid\":456,\"supervisor\":{\"pid\":123}}")]
    [InlineData("REGAIN_DIAGNOSTIC {\"version\":1,\"event\":\"usb.reset\",\"message\":\"x\",\"pid\":123,\"supervisor\":{\"pid\":\"123\"}}")]
    public void MalformedUnversionedAndForwardedWorkerRecordsAreIgnored(string line)
        => Assert.False(CameraUsbRecovery.TryParse(line, 123, true, out _));

    [Theory]
    [InlineData("usb.complete")]
    [InlineData("usb.reset.failed")]
    [InlineData("USB.reset")]
    [InlineData("usb.returned ")]
    public void OtherTransferAndSimilarEventsDoNotTriggerRecovery(string kind)
        => Assert.False(CameraUsbRecovery.TryParse(Live(123, kind), 123, true, out _));

    [Theory]
    [InlineData("42", true)]
    [InlineData("null", true)]
    [InlineData("0", false)]
    [InlineData("-1", false)]
    [InlineData("\"42\"", false)]
    public void EpisodeIdentifierMustBeANonzeroUnsignedIntegerWhenPresent(string id, bool valid)
    {
        string raw = Live(123);
        string line = raw[..^1] + $",\"details\":{{\"usbPeripheralRecoveryId\":{id}}}}}";
        Assert.Equal(valid, CameraUsbRecovery.TryParse(line, 123, true, out var recovery));
        if (valid) Assert.Equal(id == "null" ? null : (ulong?)42, recovery.RecoveryId);
    }

    [Theory]
    [InlineData("1790000000000000", true)]
    [InlineData("null", true)]
    [InlineData("0", false)]
    [InlineData("-1", false)]
    [InlineData("\"1790000000000000\"", false)]
    [InlineData("9223372036854775808", false)]
    public void ResetTimestampMustPreserveThePositiveSupervisorUnixMicroseconds(string timestamp, bool valid)
    {
        string raw = Live(123);
        string line = raw[..^1] + $",\"timestampUnixUs\":{timestamp}}}";
        Assert.Equal(valid, CameraUsbRecovery.TryParse(line, 123, true, out var recovery));
        if (valid) Assert.Equal(timestamp == "null" ? null : (long?)1790000000000000, recovery.TimestampUnixUs);
    }

    [Theory]
    [InlineData("flightRecorderBatch", "\"old-batch\"")]
    [InlineData("flightRecorderBatch", "null")]
    [InlineData("flightRecorderAfterFault", "true")]
    [InlineData("replay", "true")]
    [InlineData("replayed", "false")]
    [InlineData("managedContext", "{}")]
    public void ReplayAndAlreadyRoutedRecordsAreIgnored(string key, string value)
    {
        string raw = Live(123);
        string marked = raw[..^1] + $",\"{key}\":{value}}}";
        Assert.False(CameraUsbRecovery.TryParse(marked, 123, true, out _));
        string wrapped = raw.Replace("\"cameraSlot\":null", $"\"cameraSlot\":null,\"details\":{{\"flightRecorderBatch\":{value}}}");
        Assert.False(CameraUsbRecovery.TryParse(wrapped, 123, true, out _));
    }

    [Theory]
    [InlineData("off")]
    [InlineData("circular")]
    [InlineData("verbose")]
    public void HostLifecycleBypassesLoggingModesAndSubscriberFailures(string mode)
    {
        using var host = new HostClient(Supervisor, "unused", simulate: true, direct: true, supervised: true);
        host.ConfigureDiagnostics(mode);
        var phases = new List<CameraUsbRecoveryEvent>();
        host.UsbRecovery += _ => throw new IOException("frontend unavailable");
        host.UsbRecovery += phases.Add;
        host.ReceiveStandardError(Live(host.ProcessId));
        host.ReceiveStandardError(Live(host.ProcessId, "usb.returned"));
        Assert.Equal([CameraUsbRecoveryPhase.Resetting, CameraUsbRecoveryPhase.CameraReturned], phases.Select(item => item.Phase));
        host.RecordDiagnostic(Live(host.ProcessId));
        Assert.Equal(2, phases.Count); // Manually routed logging is never a live event.
        host.Dispose();
        host.ReceiveStandardError(Live(123));
        Assert.Equal(2, phases.Count);
    }

    [Fact]
    public void RawSdkOrDirectWorkerCannotEmitSupervisorLifecycle()
    {
        using var host = new HostClient(Worker, "unused", simulate: true, direct: true);
        var phases = new List<CameraUsbRecoveryEvent>();
        host.UsbRecovery += phases.Add;
        host.ReceiveStandardError(Live(host.ProcessId));
        Assert.Empty(phases);
    }

    [Fact]
    public async Task SessionForwardsOnlyItsCurrentHostAndWithdrawsSubscriptionOnDisconnect()
    {
        HostClient? active = null;
        using var session = new CameraSession(Camera,
            () => active = new(Supervisor, "unused", simulate: true, direct: true, supervised: true), Fast);
        var phases = new ConcurrentQueue<CameraUsbRecoveryEvent>();
        session.UsbRecovery += _ => throw new IOException("bad subscriber");
        session.UsbRecovery += phases.Enqueue;
        await session.ConnectAsync(default);
        var old = active!;
        old.ReceiveStandardError(Live(old.ProcessId));
        Assert.Equal([CameraUsbRecoveryPhase.Resetting], phases.Select(item => item.Phase));
        old.Dispose();
        await Assert.ThrowsAsync<ObjectDisposedException>(() => session.RefreshAsync(default));
        await session.ConnectAsync(default);
        Assert.NotSame(old, active);
        old.ReceiveStandardError(Live(active!.ProcessId, "usb.returned"));
        Assert.Single(phases);
        active.ReceiveStandardError(Live(active.ProcessId, "usb.returned"));
        Assert.Equal([CameraUsbRecoveryPhase.Resetting, CameraUsbRecoveryPhase.CameraReturned], phases.Select(item => item.Phase));
        session.Dispose();
        active.ReceiveStandardError(Live(123));
        Assert.Equal(2, phases.Count);
    }

    [Fact]
    public async Task PeripheralCallbackHoldsReplacementExposureUntilExplicitlyReady()
    {
        HostClient? host = null;
        var entered = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var release = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var returned = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var lifecycle = new ConcurrentQueue<CameraUsbRecoveryEvent>();
        int callbacks = 0;
        ulong callbackId = 0;
        using var session = new CameraSession(Camera,
            () => host = new(Supervisor, "unused", simulate: true, direct: true, supervised: true),
            Fast with { UsbResetAfterFailures = 1 }) {
            RecoverUsbPeripherals = async (id, token) => {
                callbackId = id;
                Interlocked.Increment(ref callbacks);
                entered.TrySetResult();
                await release.Task.WaitAsync(token);
            }
        };
        session.UsbRecovery += recovery => {
            lifecycle.Enqueue(recovery);
            if (recovery.Phase == CameraUsbRecoveryPhase.CameraReturned) returned.TrySetResult();
        };
        await session.ConnectAsync(default);
        await host!.CallAsync("simulate-read-failures", new { count = 3 }, TimeSpan.FromSeconds(15), default);
        var capture = session.CaptureAsync(new(64, 64, 1, 0, 0, 300000, true), default);
        await entered.Task.WaitAsync(TimeSpan.FromSeconds(15));
        await returned.Task.WaitAsync(TimeSpan.FromSeconds(5));
        var waiting = (await host.CallAsync("status", null, TimeSpan.FromSeconds(5), default)).Result;
        Assert.Equal("Waiting for NINA peripherals", waiting.GetProperty("phase").GetString());
        Assert.True(callbackId > 0);
        Assert.Equal(callbackId, waiting.GetProperty("usbPeripheralRecoveryId").GetUInt64());
        Assert.Equal([CameraUsbRecoveryPhase.Resetting, CameraUsbRecoveryPhase.CameraReturned], lifecycle.Select(item => item.Phase));
        Assert.All(lifecycle, item => Assert.Equal(callbackId, item.RecoveryId));
        Assert.All(lifecycle, item => Assert.True(item.TimestampUnixUs > 0));
        Assert.True(lifecycle.First().TimestampUnixUs <= lifecycle.Last().TimestampUnixUs);
        Assert.False(capture.IsCompleted);
        release.TrySetResult();
        Assert.Equal(4096, (await capture.WaitAsync(TimeSpan.FromSeconds(15))).Pixels.Length);
        Assert.Equal(1, callbacks);
    }

    [Fact]
    public async Task FailedPeripheralCallbackPreventsReplacementAndLaterCaptureUntilReconnect()
    {
        HostClient? host = null;
        int callbacks = 0;
        using var session = new CameraSession(Camera,
            () => host = new(Supervisor, "unused", simulate: true, direct: true, supervised: true),
            Fast with { UsbResetAfterFailures = 1 }) {
            RecoverUsbPeripherals = (_, _) => {
                callbacks++;
                return Task.FromException(new IOException("filter wheel did not recover"));
            }
        };
        await session.ConnectAsync(default);
        await host!.CallAsync("simulate-read-failures", new { count = 3 }, TimeSpan.FromSeconds(15), default);
        var failure = await Assert.ThrowsAsync<IOException>(() => session.CaptureAsync(new(64, 64, 1, 0, 0, 300000, true), default));
        Assert.Contains("filter wheel did not recover", failure.Message);
        Assert.Equal(1, callbacks);
        Assert.True(host.IsAlive);
        var state = (await host.CallAsync("status", null, TimeSpan.FromSeconds(5), default)).Result;
        Assert.NotEqual(2, state.GetProperty("state").GetInt32());
        Assert.Equal(JsonValueKind.Null, state.GetProperty("usbPeripheralRecoveryId").ValueKind);
        session.RecoverUsbPeripherals = (_, _) => Task.CompletedTask;
        var blocked = await Assert.ThrowsAnyAsync<IOException>(() => session.CaptureAsync(new(64, 64, 1, 0, 0, 300000, true), default));
        Assert.Contains("peripheral", blocked.Message, StringComparison.OrdinalIgnoreCase);
    }

    [Fact]
    public async Task CaptureCancellationReleasesPeripheralWaitWithoutAuthorizingAnExposure()
    {
        HostClient? host = null;
        var entered = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        using var cancelled = new CancellationTokenSource();
        using var session = new CameraSession(Camera,
            () => host = new(Supervisor, "unused", simulate: true, direct: true, supervised: true),
            Fast with { UsbResetAfterFailures = 1 }) {
            RecoverUsbPeripherals = async (_, token) => {
                entered.TrySetResult();
                await Task.Delay(Timeout.InfiniteTimeSpan, token);
            }
        };
        await session.ConnectAsync(default);
        await host!.CallAsync("simulate-read-failures", new { count = 3 }, TimeSpan.FromSeconds(15), default);
        var capture = session.CaptureAsync(new(64, 64, 1, 0, 0, 300000, true), cancelled.Token);
        await entered.Task.WaitAsync(TimeSpan.FromSeconds(15));
        cancelled.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => capture.WaitAsync(TimeSpan.FromSeconds(10)));
        Assert.True(host.IsAlive);
        var state = (await host.CallAsync("status", null, TimeSpan.FromSeconds(5), default)).Result;
        Assert.NotEqual(2, state.GetProperty("state").GetInt32());
        Assert.Equal(JsonValueKind.Null, state.GetProperty("usbPeripheralRecoveryId").ValueKind);
    }
}
