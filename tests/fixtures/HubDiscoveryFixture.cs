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
        await NetworkReply(editor.Description!.Value, editor.SavedConfiguration!.Value);
        await ScopedReply(editor.Description!.Value, editor.SavedConfiguration!.Value);
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
        var sourceId = editor.AddDiscoveredAlpacaSource(0);
        Check(editor.State == HubEditorState.Editing && editor.Draft.Dirty, "Catalog adoption did not revoke review");
        var draft = editor.Draft.Candidate;
        var prepared = draft.GetProperty("sources").EnumerateArray().Single(source => source.GetProperty("id").GetGuid() == sourceId);
        Check(prepared.GetProperty("backend").GetProperty("uniqueId").GetString() == "camera unit 42" &&
            prepared.GetProperty("backend").GetProperty("deviceNumber").GetUInt32() == uint.MaxValue,
            "Catalog adoption lost the address or identity pin");
        Check(editor.SavedConfiguration.Value.GetRawText() == before && server.Requests.Count == 1, "Draft adoption performed I/O or saved configuration");
        Check(await editor.ReviewAsync(), "Pinned source failed host validation");
        await editor.ApplyAsync(); await editor.ReloadAsync();
        var saved = editor.SavedConfiguration!.Value.GetProperty("sources").EnumerateArray().Single(source => source.GetProperty("id").GetGuid() == sourceId);
        Check(saved.GetProperty("backend").GetProperty("uniqueId").GetString() == "camera unit 42", "Saved source lost its pin");
        var pinnedStatus = await editor.SourceStatusAsync(sourceId);
        Check(pinnedStatus.GetProperty("leaseCount").GetInt32() == 0 && !pinnedStatus.GetProperty("transportConnected").GetBoolean() && server.Requests.Count == 1,
            "Review/apply of a pinned source contacted equipment");
        await editor.ReloadAsync(); Check(editor.LastDiscovery is null, "Reload retained stale catalog");
    }
    // Run in both .NET 8 and real net48 x86/x64 clients without broadcasting
    // on the user's LAN. Exercise IP canonicalization and zone preservation.
    private static async Task NetworkReply(JsonElement description, JsonElement saved)
    {
        var revision=saved.GetProperty("revision").GetGuid(); var queries=0;
        using var search=new HubEditorSession(saved.GetProperty("instanceId").GetGuid(), (command, _) => {
            switch (command.GetProperty("op").GetString()) {
                case "describeConfig": return Task.FromResult(description);
                case "getConfig": return Task.FromResult(saved);
                case "hostStatus": return Task.FromResult(JsonSerializer.SerializeToElement(new {phase="ready",configurationRevision=revision}));
                case "searchAlpaca":
                    queries++; Check(command.EnumerateObject().Count()==2 && command.GetProperty("expectedRevision").GetGuid()==revision,"Search changed its inputs");
                    return Task.FromResult(JsonSerializer.SerializeToElement(new {
                        configurationRevision=revision,interfacesTried=3,interfacesFailed=0,ignoredDatagrams=0,incomplete=false,
                        servers=new object[] {
                            new {address="192.0.2.3",scopeId=0,port=80,baseUrl="http://192.0.2.3:80"},
                            new {address="2001:db8::42",scopeId=0,port=11111,baseUrl="http://[2001:db8::42]:11111"},
                            new {address="fe80::42",scopeId=7,port=11111,baseUrl="http://[fe80::42]:11111"},
                            new {address="::c000:203",scopeId=0,port=1,baseUrl="http://[::c000:203]:1"}
                        }
                    }));
                default: throw new InvalidOperationException("Network search performed other I/O");
            }
        },()=>{});
        await search.ReloadAsync(); var result=await search.SearchAlpacaAsync();
        Check(queries==1 && !search.Draft!.Dirty && search.LastDiscovery is null,"Network search changed configuration or read a catalog");
        Check(result.GetProperty("servers")[2].GetProperty("scopeId").GetUInt32()==7 && result.GetProperty("servers")[2].GetProperty("baseUrl").GetString()=="http://[fe80::42]:11111","Network search erased an IPv6 zone");
    }
    private static async Task ScopedReply(JsonElement description, JsonElement saved)
    {
        var revision = saved.GetProperty("revision").GetGuid(); var queries = 0;
        using var editor = new HubEditorSession(saved.GetProperty("instanceId").GetGuid(), (command, _) => {
            switch (command.GetProperty("op").GetString()) {
                case "describeConfig": return Task.FromResult(description);
                case "getConfig": return Task.FromResult(saved);
                case "hostStatus": return Task.FromResult(JsonSerializer.SerializeToElement(new { phase = "ready", configurationRevision = revision }));
                case "discoverAlpaca":
                    queries++; Check(command.GetProperty("scopeId").GetUInt32() == 7, "Catalog query lost its interface");
                    return Task.FromResult(JsonSerializer.SerializeToElement(new {
                        configurationRevision = revision, baseUrl = "http://[fe80::42]:11111", scopeId = 7,
                        devices = new[] { new { name = "[SIMULATION] scoped camera", reportedDeviceType = "Camera", supportedDeviceType = "camera", number = 0, uniqueId = "scoped camera" } }
                    }));
                default: throw new InvalidOperationException("Scoped catalog performed other I/O");
            }
        }, () => { });
        await editor.ReloadAsync();
        await editor.DiscoverAlpacaScopedAsync("http://[fe80::42]:11111", 7);
        var id = editor.AddDiscoveredAlpacaSource(0);
        var source = editor.Draft!.Candidate.GetProperty("sources").EnumerateArray().Single(s => s.GetProperty("id").GetGuid() == id);
        Check(queries == 1 && source.GetProperty("backend").GetProperty("scopeId").GetUInt32() == 7, "Scoped adoption lost its interface or repeated a query");
    }
}
