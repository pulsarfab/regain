using System.Collections.Concurrent;
using ZwoGain.Core;
using Xunit;

namespace ZwoGain.NINA.Tests;

public class CameraLogTests
{
    [Theory]
    [InlineData("warning", CameraLog.Level.Warning)]
    [InlineData("error", CameraLog.Level.Warning)]
    [InlineData("info", CameraLog.Level.Info)]
    [InlineData("debug", CameraLog.Level.Debug)]
    internal void WorkerRecordsUseLogLevelsWithoutOperatorErrors(string level, CameraLog.Level expected)
    {
        var record = CameraLog.Parse("direct", "ZWOGAIN_DIAGNOSTIC " +
            System.Text.Json.JsonSerializer.Serialize(new { version = 1, level, @event = "transfer.retry", message = "USB timeout\nretry 1/2", pid = 42 }));
        Assert.Equal(expected, record.Severity);
        Assert.Contains("ZWOgain direct worker 42 [transfer.retry]", record.Text);
        Assert.DoesNotContain('\n', record.Text);
        var routed = new List<CameraLog.Level>();
        CameraLog.Forward(record, _ => routed.Add(CameraLog.Level.Info), _ => routed.Add(CameraLog.Level.Warning), _ => routed.Add(CameraLog.Level.Debug));
        Assert.Equal([expected], routed);
        CameraLog.Forward(record, _ => throw new IOException(), _ => throw new IOException(), _ => throw new IOException());
    }

    [Theory]
    [InlineData("Error: load ASICamera2 failed")]
    [InlineData("ZWOGAIN_DIAGNOSTIC {broken")]
    [InlineData("ZWOGAIN_DIAGNOSTIC {\"version\":1,\"message\":false}")]
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
        var received = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        string root = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../"));
        HostClient Factory()
        {
            var host = new HostClient(Path.Combine(root, "target/debug/zwogain-direct.exe"), "unused", simulate: true, direct: true, log: line => {
                var record = CameraLog.Parse("direct", line);
                CameraLog.Forward(record, _ => { }, text => {
                    warnings.Enqueue(text);
                    if (warnings.Count == 2) received.TrySetResult();
                }, _ => { });
                if (brokenLogger) throw new IOException("log file unavailable");
            });
            host.CallAsync("simulate-read-failures", new { count = 2 }, TimeSpan.FromSeconds(3), default).GetAwaiter().GetResult();
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
    }
}
