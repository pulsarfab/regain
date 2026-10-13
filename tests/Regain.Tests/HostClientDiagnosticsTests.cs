using System.Diagnostics;
using System.Text.Json;
using Regain.Core;
using Xunit;

namespace Regain.Tests;

public class HostClientDiagnosticsTests
{
    [Fact]
    public void CircularModeIsDefaultAndFlushesManagedHistoryOnAFault()
    {
        var lines = new List<string>();
        var diagnostics = new HostClientDiagnostics(lines.Add, "direct", 731, true);
        Assert.Equal("circular", diagnostics.TransferLogMode);
        diagnostics.Complete(1, "download", 122342976, Stopwatch.GetTimestamp()); Assert.Empty(lines);
        diagnostics.Worker(Prefix+"{\"version\":1,\"event\":\"transfer.retry\",\"level\":\"warning\",\"message\":\"failed\"}");
        Assert.Contains(lines, line=>Parse(line).GetProperty("event").GetString()=="diagnostics.batch.begin");
        Assert.Contains(lines, line=>Parse(line).GetProperty("event").GetString()=="ipc.command.complete" && Parse(line).TryGetProperty("flightRecorderBatch",out _));
        diagnostics.TransferLogMode="verbose";
        int prior=lines.Count; diagnostics.Complete(2,"download",100,Stopwatch.GetTimestamp()); Assert.Equal(prior+1,lines.Count);
        diagnostics.TransferLogMode="off";
        diagnostics.Complete(3,"download",100,Stopwatch.GetTimestamp()); Assert.Equal(prior+1,lines.Count);
    }
    [Fact]
    public void CameraLoggingSettingsAreIsolatedAndKeepFailureRecords()
    {
        var quiet = new List<string>();
        var verbose = new List<string>();
        var first = new HostClientDiagnostics(quiet.Add, "direct", 1, true) { TransferLogMode = "off" };
        var second = new HostClientDiagnostics(verbose.Add, "sdk", 2, true) { TransferLogMode = "verbose" };
        foreach (var diagnostics in new[] { first, second }) {
            diagnostics.Complete(1, "download", 122342976, Stopwatch.GetTimestamp());
            diagnostics.Worker(Prefix + "{\"version\":1,\"event\":\"usb.complete\",\"message\":\"read\",\"level\":\"info\"}");
            diagnostics.Worker(Prefix + "{\"version\":1,\"event\":\"usb.failed\",\"message\":\"failure\",\"level\":\"warning\"}");
            diagnostics.Failed(2, "download", new IOException(), Stopwatch.GetTimestamp());
        }
        Assert.Equal(["usb.failed", "ipc.command.failed"], quiet.Select(line => Parse(line).GetProperty("event").GetString()));
        Assert.Equal(["ipc.command.complete", "usb.complete", "usb.failed", "ipc.command.failed"], verbose.Select(line => Parse(line).GetProperty("event").GetString()));
        Assert.Equal("warning", Parse(quiet[^1]).GetProperty("level").GetString());
        Assert.Equal("warning", Parse(verbose[^1]).GetProperty("level").GetString());
    }

    [Fact]
    public async Task RetiringWorkerDrainsQueuedNativeFailureBeforeDiscardingStderrReader()
    {
        string worker = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../target/debug/regain-device.exe"));
        var blocked = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var release = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var nativeFailure = new TaskCompletionSource<string>(TaskCreationOptions.RunContinuationsAsynchronously);
        int blockedOnce = 0;
        using var host = new HostClient(worker, "unused", simulate: true, log: line => {
            if (!line.StartsWith(Prefix, StringComparison.Ordinal)) return;
            var record = Parse(line);
            string? kind = record.GetProperty("event").GetString();
            if (kind == "diagnostics.manual" && Interlocked.Exchange(ref blockedOnce, 1) == 0) {
                blocked.TrySetResult();
                release.Task.GetAwaiter().GetResult();
            }
            if (kind == "command.failed") nativeFailure.TrySetResult(line);
        });
        Task? retiring = null;
        try {
            var timeout = TimeSpan.FromSeconds(15);
            await host.CallAsync("open", new { name = "ZWO Simulated" }, timeout, default);
            // Block an earlier native record, leaving the later failure queued
            // behind it. Tiny metadata still fits the worker's stderr pipe.
            await host.CallAsync("diagnostics-dump", null, timeout, default);
            await blocked.Task.WaitAsync(timeout);
            await host.CallAsync("fault", new { kind = "download" }, timeout, default);
            await host.CallAsync("start", new { width = 960, height = 640, bin = 1,
                x = 0, y = 0, microseconds = 1000, dark = false }, timeout, default);
            await Assert.ThrowsAsync<SdkException>(() => host.CallAsync("download", null, timeout, default));
            Assert.False(nativeFailure.Task.IsCompleted);
            using var process = Process.GetProcessById(host.ProcessId);
            retiring = Task.Run(host.Dispose);
            await process.WaitForExitAsync().WaitAsync(timeout);
            // Process exit alone is insufficient: retirement must remain behind
            // the deliberately held callback until queued records reach the sink.
            await Task.WhenAny(retiring, Task.Delay(200));
            Assert.False(retiring.IsCompleted, "Worker retirement discarded its queued stderr before EOF");
            release.TrySetResult();
            await retiring.WaitAsync(timeout);
            Assert.True(nativeFailure.Task.IsCompletedSuccessfully);
            var failure = Parse(await nativeFailure.Task);
            Assert.Equal("warning", failure.GetProperty("level").GetString());
            Assert.Contains("ASI error 11", failure.GetProperty("message").GetString());
        }
        finally {
            release.TrySetResult();
            if (retiring is not null) await retiring.WaitAsync(TimeSpan.FromSeconds(15));
        }
    }
    private const string Prefix = "REGAIN_DIAGNOSTIC ";
    private static JsonElement Parse(string line) => JsonDocument.Parse(line[Prefix.Length..]).RootElement.Clone();

    [Fact]
    public void WorkerFieldsRemainAuthoritativeAndReceiptContextIsSeparate()
    {
        var lines = new List<string>();
        var diagnostics = new HostClientDiagnostics(lines.Add, "direct", 123, supervised: true) { TransferLogMode = "verbose" };
        diagnostics.Begin(7, "open", JsonSerializer.SerializeToElement(new { name = "ASI6200MM Pro", serial = "private-identity" }), 100);
        diagnostics.Begin(8, "start", JsonSerializer.SerializeToElement(new { pixels = "private-image-data" }), 50);
        var original = JsonSerializer.SerializeToElement(new {
            version = 1, level = "info", @event = "usb.complete", message = "complete", pid = 456,
            backend = "worker-sdk", id = 99, capture = 88, correlation = 77,
            elapsedUs = 123.25, timestampUnixUs = 9876543210L,
            details = new { status = 0, request = "0xbc", receivedBytes = 1024 }, futureField = "preserved"
        });
        diagnostics.Worker(Prefix + original.GetRawText());
        var record = Parse(lines[^1]);
        foreach (var property in original.EnumerateObject())
            Assert.Equal(property.Value.GetRawText(), record.GetProperty(property.Name).GetRawText());
        var context = record.GetProperty("managedContext");
        Assert.Equal(123, context.GetProperty("hostPid").GetInt32());
        Assert.Equal("direct", context.GetProperty("backend").GetString());
        Assert.Equal("ASI6200MM Pro", context.GetProperty("camera").GetString());
        Assert.Equal(8, context.GetProperty("requestId").GetInt64());
        Assert.Equal(1, context.GetProperty("capture").GetInt64());
        Assert.DoesNotContain(lines, line => line.Contains("private-identity") || line.Contains("private-image-data"));
    }

    [Fact]
    public void CommandTimingSeparatesResponseFromPixelDeliveryAndRetainsCorrelation()
    {
        var lines = new List<string>();
        var diagnostics = new HostClientDiagnostics(lines.Add, "direct", 123, supervised: true) { TransferLogMode = "verbose" };
        long started = Stopwatch.GetTimestamp();
        diagnostics.Begin(1, "start", JsonSerializer.SerializeToElement(new { }), 70);
        diagnostics.Response(1, "start", JsonSerializer.SerializeToElement(new { ok = true, result = new { backend = "sdk", secret = "private-response" } }), 200, 0, started);
        diagnostics.Complete(1, "start", 0, started);
        diagnostics.Begin(2, "download", JsonSerializer.SerializeToElement(new { }), 60);
        diagnostics.Response(2, "download", JsonSerializer.SerializeToElement(new { ok = true }), 150, 122342976, started);
        diagnostics.Complete(2, "download", 122342976, started);
        Assert.Equal(["ipc.command.submit", "ipc.command.response", "ipc.command.complete", "ipc.command.submit", "ipc.command.response", "ipc.command.complete"],
            lines.Select(line => Parse(line).GetProperty("event").GetString()));
        var response = Parse(lines[4]);
        var complete = Parse(lines[5]);
        Assert.Equal(122342976, complete.GetProperty("details").GetProperty("binaryBytes").GetInt32());
        Assert.True(complete.GetProperty("elapsedUs").GetDouble() >= response.GetProperty("elapsedUs").GetDouble());
        Assert.True(complete.GetProperty("timestampUnixUs").GetInt64() > 0);
        Assert.Equal(response.GetProperty("managedContext").GetProperty("correlation").GetString(),
            complete.GetProperty("managedContext").GetProperty("correlation").GetString());
        Assert.Equal("sdk", complete.GetProperty("managedContext").GetProperty("backend").GetString());
        Assert.Equal(1, complete.GetProperty("managedContext").GetProperty("capture").GetInt64());
        Assert.DoesNotContain(lines, line => line.Contains("private-response"));
    }

    [Fact]
    public void ExistingManagedContextAndUnstructuredLinesArePreserved()
    {
        var lines = new List<string>();
        var diagnostics = new HostClientDiagnostics(lines.Add, "sdk", 123, supervised: false) { TransferLogMode = "verbose" };
        diagnostics.Worker(Prefix + "{\"version\":1,\"managedContext\":{\"future\":42},\"details\":{\"status\":0}}");
        Assert.Equal(42, Parse(lines[0]).GetProperty("managedContext").GetProperty("future").GetInt32());
        diagnostics.Worker(Prefix + "{broken");
        diagnostics.Worker("SDK loader unavailable");
        Assert.Equal(Prefix + "{broken", lines[1]);
        Assert.Equal("SDK loader unavailable", lines[2]);
    }

    [Fact]
    public void FailedLogSinkCannotChangeCommandsOrWorkerDiagnostics()
    {
        var diagnostics = new HostClientDiagnostics(_ => throw new IOException("log sink unavailable"), "direct", 123, supervised: true) { TransferLogMode = "verbose" };
        diagnostics.Begin(1, "start", JsonSerializer.SerializeToElement(new { }), 70);
        diagnostics.Response(1, "start", JsonSerializer.SerializeToElement(new { ok = false }), 20, 0, Stopwatch.GetTimestamp());
        diagnostics.Failed(1, "start", new SdkException("private-error", 11, "ASIStartExposure"), Stopwatch.GetTimestamp());
        diagnostics.Complete(1, "start", 0, Stopwatch.GetTimestamp());
        diagnostics.Worker(Prefix + "{\"version\":1,\"event\":\"usb.complete\",\"message\":\"done\"}");
        diagnostics.Worker("legacy");
    }
}
