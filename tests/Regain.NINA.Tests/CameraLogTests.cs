using System.Collections.Concurrent;
using Regain.Core;
using Xunit;

namespace Regain.NINA.Tests;

public class CameraLogTests
{
    [Fact]
    public void FlightRecorderReplayBypassesQuietFilteringWithoutChangingOriginalEvidence()
    {
        var line="REGAIN_DIAGNOSTIC {\"version\":1,\"level\":\"info\",\"event\":\"usb.complete\",\"message\":\"read\",\"pid\":42,\"flightRecorderBatch\":\"42:1\",\"details\":{\"correlation\":7,\"elapsedUs\":123}}";
        var record=CameraLog.Parse("direct",line,verbose:false);
        Assert.False(record.Suppressed); Assert.Contains(line,record.Text);
    }
    [Fact]
    public void CameraSettingSuppressesSuccessfulNinaTimingsButKeepsFailures()
    {
        long start = System.Diagnostics.Stopwatch.GetTimestamp();
        Assert.True(CameraLog.ApiTimingEntry("ASI6200", "direct", 1, "DownloadExposure", start,
            "success", transferLogging: false).Suppressed);
        var failure = CameraLog.ApiTimingEntry("ASI6200", "direct", 1, "DownloadExposure", start,
            "failed", new IOException(), transferLogging: false);
        Assert.False(failure.Suppressed);
        Assert.Equal(CameraLog.Level.Warning, failure.Severity);
    }
    [Theory]
    [InlineData("usb.complete", "info", true)]
    [InlineData("sdk.api.start", "info", true)]
    [InlineData("ipc.command.complete", "info", true)]
    [InlineData("nina.image_factory.complete", "info", true)]
    [InlineData("usb.failed", "warning", false)]
    [InlineData("capture.failed", "error", false)]
    [InlineData("transfer.retry", "warning", false)]
    [InlineData("capture.phase", "info", false)]
    [InlineData("ipc.command.failed", "info", false)]
    public void QuietLoggingSuppressesCommandVolumeButRetainsFailuresAndPhases(string kind, string level, bool suppressed)
    {
        var line = "REGAIN_DIAGNOSTIC " + System.Text.Json.JsonSerializer.Serialize(new { version = 1, level, @event = kind, message = "record", pid = 42 });
        var record = CameraLog.Parse("direct", line, verbose: false);
        Assert.Equal(suppressed, record.Suppressed);
        var routed = new List<string>();
        CameraLog.Forward(record, routed.Add, routed.Add, routed.Add);
        Assert.Equal(suppressed, routed.Count == 0);
        Assert.False(CameraLog.Parse("direct", line, verbose: true).Suppressed);
    }
    [Theory]
    [InlineData("warning", CameraLog.Level.Warning)]
    [InlineData("error", CameraLog.Level.Warning)]
    [InlineData("info", CameraLog.Level.Info)]
    [InlineData("debug", CameraLog.Level.Debug)]
    internal void WorkerRecordsUseLogLevelsWithoutOperatorErrors(string level, CameraLog.Level expected)
    {
        var record = CameraLog.Parse("direct", "REGAIN_DIAGNOSTIC " +
            System.Text.Json.JsonSerializer.Serialize(new { version = 1, level, @event = "transfer.retry", message = "USB timeout\nretry 1/2", pid = 42 }));
        Assert.Equal(expected, record.Severity);
        Assert.Contains("PulsarFab regain direct process 42 [transfer.retry]", record.Text);
        Assert.DoesNotContain('\n', record.Text);
        var routed = new List<CameraLog.Level>();
        CameraLog.Forward(record, _ => routed.Add(CameraLog.Level.Info), _ => routed.Add(CameraLog.Level.Warning), _ => routed.Add(CameraLog.Level.Debug));
        Assert.Equal(expected == CameraLog.Level.Warning ? [CameraLog.Level.Info, CameraLog.Level.Warning] : [CameraLog.Level.Info], routed);
        CameraLog.Forward(record, _ => throw new IOException(), _ => throw new IOException(), _ => throw new IOException());
    }

    [Fact]
    public void StructuredTransferFieldsAndLongRecordsSurviveInfoLogging()
    {
        var line = "REGAIN_DIAGNOSTIC " + System.Text.Json.JsonSerializer.Serialize(new {
            version = 1, level = "debug", @event = "usb.complete", message = "Bulk transfer completed", pid = 42,
            backend = "sdk", camera = "ASI6200MM Pro", capture = 12, correlation = 87,
            timestampUnixUs = 1234567890123L, elapsedUs = 287.5,
            details = new { request = "0xbc", wValue = "0x0040", requestedBytes = 1048576,
                receivedBytes = 1048576, status = 0, vendorExtension = new string('x', 9000) }
        });
        var record = CameraLog.Parse("SDK", line);
        string? info = null;
        CameraLog.Forward(record, text => info = text, _ => throw new Exception(), _ => throw new Exception());
        Assert.NotNull(info);
        Assert.Contains(line, info);
        Assert.DoesNotContain("[truncated]", info);
        var json = info[(info.IndexOf("REGAIN_DIAGNOSTIC ", StringComparison.Ordinal) + "REGAIN_DIAGNOSTIC ".Length)..];
        using var document = System.Text.Json.JsonDocument.Parse(json);
        var root = document.RootElement;
        Assert.Equal(87, root.GetProperty("correlation").GetInt32());
        Assert.Equal(287.5, root.GetProperty("elapsedUs").GetDouble());
        Assert.Equal(9000, root.GetProperty("details").GetProperty("vendorExtension").GetString()!.Length);
    }

    [Fact]
    public void FailedInfoSinkDoesNotSuppressWarningAndLegacyNoiseRemainsBounded()
    {
        var record = CameraLog.Parse("direct", "REGAIN_DIAGNOSTIC {\"version\":1,\"level\":\"warning\",\"event\":\"usb.failed\",\"message\":\"timeout\"}");
        string? warning = null;
        CameraLog.Forward(record, _ => throw new IOException(), text => warning = text, _ => throw new Exception());
        Assert.Equal(record.Text, warning);
        var legacy = CameraLog.Parse("SDK", new string('x', 10000));
        Assert.Contains("[truncated]", legacy.Text);
        Assert.True(legacy.Text.Length < 8300);
    }

    [Theory]
    [InlineData("WaitUntilExposureIsReady", "success", "nina.api.complete")]
    [InlineData("WaitUntilExposureIsReady", "cancelled", "nina.api.complete")]
    [InlineData("DownloadExposure", "aborted", "nina.api.complete")]
    [InlineData("CreateImageArrayExposureData", "failed", "nina.image_factory.complete")]
    public void NinaApiTimingsAreBoundedMetadataAtInfo(string api, string outcome, string kind)
    {
        var record = CameraLog.ApiTimingEntry(new string('c', 10000), "direct", 7, api,
            System.Diagnostics.Stopwatch.GetTimestamp(), outcome, new IOException("private-error-payload"),
            9576, 6388, 61171488);
        string? info = null;
        CameraLog.Forward(record, text => info = text, _ => throw new Exception(), _ => throw new Exception());
        Assert.NotNull(info);
        Assert.True(info.Length < 3000);
        Assert.DoesNotContain("private-error-payload", info);
        var json = info[(info.IndexOf("REGAIN_DIAGNOSTIC ", StringComparison.Ordinal) + "REGAIN_DIAGNOSTIC ".Length)..];
        using var document = System.Text.Json.JsonDocument.Parse(json);
        var root = document.RootElement;
        Assert.Equal(kind, root.GetProperty("event").GetString());
        Assert.Equal(7, root.GetProperty("capture").GetInt64());
        Assert.Equal(256, root.GetProperty("camera").GetString()!.Length);
        var details = root.GetProperty("details");
        Assert.Equal(api, details.GetProperty("api").GetString());
        Assert.Equal(outcome, details.GetProperty("outcome").GetString());
        Assert.True(details.GetProperty("elapsedUs").GetDouble() >= 0);
        Assert.Equal(61171488, details.GetProperty("pixelCount").GetInt32());
        Assert.Equal("IOException", details.GetProperty("errorType").GetString());
    }

    [Theory]
    [InlineData("Error: load ASICamera2 failed")]
    [InlineData("REGAIN_DIAGNOSTIC {broken")]
    [InlineData("REGAIN_DIAGNOSTIC {\"version\":1,\"message\":false}")]
    public void LoaderAndMalformedMessagesRemainVisible(string line)
    {
        var record = CameraLog.Parse("SDK", line);
        Assert.Equal(CameraLog.Level.Info, record.Severity);
        Assert.Contains(line, record.Text);
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task LiveWorkerRetryLogsDoNotChangeTheCaptureEvenWhenALogSinkFails(bool brokenLogger)
    {
        var warnings = new ConcurrentQueue<string>();
        var diagnostics = new ConcurrentQueue<string>();
        var workerRecords = new ConcurrentQueue<string>();
        var received = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        string root = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../"));
        HostClient Factory()
        {
            var host = new HostClient(Path.Combine(root, "target/debug/regain-device.exe"), "unused", simulate: true, direct: true, log: line => {
                workerRecords.Enqueue(line);
                var record = CameraLog.Parse("direct", line);
                CameraLog.Forward(record, _ => { }, text => {
                    warnings.Enqueue(text);
                    if (text.Contains("[transfer.retry]", StringComparison.Ordinal) && text.Contains("retry 2/2", StringComparison.Ordinal)) received.TrySetResult();
                }, _ => { });
                if (brokenLogger) throw new IOException("log file unavailable");
            });
            // A cold worker launch can take several seconds on shared CI machines.
            host.CallAsync("simulate-read-failures", new { count = 2 }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
            return host;
        }
        using var session = new CameraSession(new("ZWO ASI676MC", 3552, 3552, true, 0, 2, 12, false, false, [1]), Factory,
            new() { MaxRetries = 0, DirectReadRetries = 2, ReconnectDelaySeconds = .05 });
        if (brokenLogger) session.Diagnostic += _ => throw new IOException("session log unavailable");
        session.Diagnostic += diagnostics.Enqueue;
        await session.ConnectAsync(default);
        var frame = await session.CaptureAsync(new(64, 64, 1, 0, 0, 10000, true), default);
        await received.Task.WaitAsync(TimeSpan.FromSeconds(3));
        Assert.Equal(4096, frame.Pixels.Length);
        Assert.Equal(0, frame.Recoveries);
        Assert.Equal(2, frame.RetainedReadRecoveries);
        Assert.Contains(warnings, text => text.Contains("[transfer.retry]") && text.Contains("retry 1/2"));
        Assert.Contains(warnings, text => text.Contains("retry 2/2") && text.Contains("simulated USB read failure"));
        Assert.Contains(diagnostics, text => text.Contains("Recovered retained frame"));
        Assert.DoesNotContain(diagnostics, text => text.StartsWith("Capture failed"));
        Assert.Contains(workerRecords, line => line.Contains("\"event\":\"ipc.command.complete\"")
            && line.Contains("\"method\":\"download\"") && line.Contains("\"binaryBytes\":8192"));
        Assert.Contains(workerRecords, line => line.Contains("\"managedContext\":")
            && line.Contains("\"camera\":\"ZWO ASI676MC\"") && line.Contains("\"capture\":1"));
    }
}
