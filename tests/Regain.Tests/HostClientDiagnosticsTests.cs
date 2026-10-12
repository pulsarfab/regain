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
