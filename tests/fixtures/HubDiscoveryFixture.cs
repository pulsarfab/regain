using System.Collections.Concurrent;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using Regain.Hub;

namespace Regain.TestFixtures;

// A private management-only peer. Every device request is recorded and rejected;
// no physical equipment, installed driver or general network scan is used.
internal sealed class HubCatalogServer : IDisposable
{
    private readonly TcpListener listener = new(IPAddress.Loopback, 0);
    private readonly CancellationTokenSource stop = new();
    private readonly Task serving;
    internal readonly ConcurrentQueue<string> Requests = new();
    internal string Url { get; }
    internal HubCatalogServer()
    {
        listener.Start(); Url = "http://127.0.0.1:" + ((IPEndPoint)listener.LocalEndpoint).Port + "/prefix/";
        serving = Task.Factory.StartNew(() => {
            while (!stop.IsCancellationRequested) {
                try {
                    using var client = listener.AcceptTcpClient();
                    client.NoDelay = true; client.ReceiveTimeout = 5000; client.SendTimeout = 5000;
                    using var stream = client.GetStream();
                    using var reader = new StreamReader(stream, Encoding.ASCII, false, 4096, true);
                    var request = reader.ReadLine() ?? ""; Requests.Enqueue(request);
                    while (!string.IsNullOrEmpty(reader.ReadLine())) { }
                    var management = request.StartsWith("GET /prefix/management/v1/configureddevices?", StringComparison.Ordinal);
                    var response = management ? JsonSerializer.Serialize(new {
                        ErrorNumber = 0, ClientTransactionID = 1, ServerTransactionID = 7,
                        Value = new[] {
                            new { DeviceName = "[SIMULATION] café camera", DeviceType = "Camera", DeviceNumber = uint.MaxValue, UniqueID = "camera unit 42" },
                            new { DeviceName = "[SIMULATION] mount", DeviceType = "Telescope", DeviceNumber = 9u, UniqueID = "mount-99" }
                        }
                    }) : "{}";
                    var bytes = Encoding.UTF8.GetBytes(response);
                    var headers = Encoding.ASCII.GetBytes("HTTP/1.1 " + (management ? "200 OK" : "404 Not Found") +
                        "\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: " + bytes.Length + "\r\n\r\n");
                    stream.Write(headers, 0, headers.Length); stream.Write(bytes, 0, bytes.Length);
                } catch (Exception error) when (stop.IsCancellationRequested && error is SocketException or ObjectDisposedException or IOException) { }
            }
        }, CancellationToken.None, TaskCreationOptions.LongRunning, TaskScheduler.Default);
    }
    public void Dispose()
    {
        stop.Cancel(); listener.Stop();
        if (!serving.Wait(TimeSpan.FromSeconds(6))) throw new TimeoutException("Private catalog peer did not stop");
        stop.Dispose();
    }
}

internal static class HubDiscoveryFixture
{
    private static void Check(bool value, string message) { if (!value) throw new InvalidOperationException(message); }
    internal static async Task Run(HubEditorSession editor)
    {
        using var server = new HubCatalogServer();
        await editor.ReloadAsync();
        var before = editor.SavedConfiguration!.Value.GetRawText();
        Check(await editor.ReviewAsync(), "Private configuration did not review");
        foreach (var invalid in new[] { "file:///secret", "http://user:secret@localhost/", "http://localhost/?secret", "http://localhost/#secret" }) {
            var rejected = false;
            try { await editor.DiscoverAlpacaAsync(invalid); } catch (InvalidOperationException) { rejected = true; }
            Check(rejected, "Invalid catalog URL was accepted");
        }
        Check(server.Requests.IsEmpty, "Invalid URLs performed network I/O");
        var result = await editor.DiscoverAlpacaAsync(server.Url);
        Check(result.GetProperty("configurationRevision").GetGuid() == editor.Draft!.Revision, "Catalog revision changed");
        var entries = result.GetProperty("devices");
        Check(entries.GetArrayLength() == 2 && entries[0].GetProperty("number").GetUInt32() == uint.MaxValue &&
            entries[0].GetProperty("uniqueId").GetString() == "camera unit 42" && entries[0].GetProperty("supportedDeviceType").GetString() == "camera" &&
            entries[1].GetProperty("supportedDeviceType").ValueKind == JsonValueKind.Null, "Catalog identities/support were changed");
        Check(editor.State == HubEditorState.Reviewed && !editor.Draft.Dirty, "Catalog read invalidated or changed reviewed draft");
        Check(editor.LastDiscovery!.Value.GetRawText() == result.GetRawText(), "Catalog was not retained");
        Check(editor.SavedConfiguration.Value.GetRawText() == before, "Catalog read modified configuration");
        foreach (var source in editor.SavedConfiguration.Value.GetProperty("sources").EnumerateArray()) {
            var status = await editor.SourceStatusAsync(source.GetProperty("id").GetGuid());
            Check(status.GetProperty("leaseCount").GetInt32() == 0 && !status.GetProperty("transportConnected").GetBoolean(), "Catalog read opened equipment");
        }
        Check(server.Requests.Count == 1, "Catalog read retried or called a device");
        await editor.ReloadAsync(); Check(editor.LastDiscovery is null, "Reload retained stale catalog");
    }
}
