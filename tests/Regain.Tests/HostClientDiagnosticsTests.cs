using System.Diagnostics;
using System.Text.Json;
using Regain.Core;
using Xunit;

namespace Regain.Tests;

public class HostClientDiagnosticsTests
{
    private const string Prefix = "REGAIN_DIAGNOSTIC ";
    private static JsonElement Parse(string line) => JsonDocument.Parse(line[Prefix.Length..]).RootElement.Clone();

    [Fact]
    public void WorkerFieldsRemainAuthoritativeAndReceiptContextIsSeparate()
    {
        var lines = new List<string>();
        var diagnostics = new HostClientDiagnostics(lines.Add, "direct", 123, supervised: true);
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
        var diagnostics = new HostClientDiagnostics(lines.Add, "direct", 123, supervised: true);
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
        var diagnostics = new HostClientDiagnostics(lines.Add, "sdk", 123, supervised: false);
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
        var diagnostics = new HostClientDiagnostics(_ => throw new IOException("log sink unavailable"), "direct", 123, supervised: true);
        diagnostics.Begin(1, "start", JsonSerializer.SerializeToElement(new { }), 70);
        diagnostics.Response(1, "start", JsonSerializer.SerializeToElement(new { ok = false }), 20, 0, Stopwatch.GetTimestamp());
        diagnostics.Failed(1, "start", new SdkException("private-error", 11, "ASIStartExposure"), Stopwatch.GetTimestamp());
        diagnostics.Complete(1, "start", 0, Stopwatch.GetTimestamp());
        diagnostics.Worker(Prefix + "{\"version\":1,\"event\":\"usb.complete\",\"message\":\"done\"}");
        diagnostics.Worker("legacy");
    }
}
