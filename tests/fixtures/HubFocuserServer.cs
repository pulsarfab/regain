using System.Collections.Concurrent;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace Regain.TestFixtures;

internal static class HubFocuserSimulation
{
    internal static Guid AddTo(JsonObject config, params int[] numbers)
    {
        var source = Guid.NewGuid();
        config["sources"]!.AsArray().Add(JsonSerializer.SerializeToNode(new {
            id = source, label = "Explicit simulation focuser", backend = new { kind = "simulated", deviceType = "focuser" },
            polling = new { pollSeconds = 0.1, requestTimeoutSeconds = 0.3 } }));
        foreach (var number in numbers) config["outputs"]!.AsArray().Add(JsonSerializer.SerializeToNode(new {
            id = Guid.NewGuid(), number, label = "Simulation focuser " + number,
            device = new { kind = "proxy", source, deviceType = "focuser" } }));
        return source;
    }
}

// Private loopback-only upstream shared by net8 NINA and real net48 COM tests.
internal sealed class HubFocuserServer : IDisposable
{
    private readonly TcpListener listener = new(IPAddress.Loopback, 0);
    private readonly CancellationTokenSource stopping = new();
    private readonly ConcurrentDictionary<TcpClient, byte> clients = new();
    private readonly ConcurrentBag<Task> requests = new();
    private readonly Task serving;
    internal readonly ConcurrentDictionary<string, object> Values = new();
    private int moves, halts, connected;
    internal int Moves => Volatile.Read(ref moves);
    internal int Halts => Volatile.Read(ref halts);
    internal volatile bool LoseMoveReply;
    internal string Url { get; }
    internal Guid SourceId { get; } = Guid.NewGuid();
    internal HubFocuserServer()
    {
        Values["absolute"] = true; Values["maxstep"] = 1000; Values["maxincrement"] = 100;
        Values["tempcompavailable"] = true; Values["tempcomp"] = true; Values["position"] = 50;
        Values["ismoving"] = false; Values["temperature"] = -5.0;
        listener.Start(); Url = "http://127.0.0.1:" + ((IPEndPoint)listener.LocalEndpoint).Port;
        serving = Serve();
    }
    internal void AddTo(JsonObject config, params uint[] numbers)
    {
        config["sources"]!.AsArray().Add(JsonSerializer.SerializeToNode(new {
            id = SourceId, label = "Private loopback focuser",
            polling = new { pollSeconds = 0.1, requestTimeoutSeconds = 0.3, attemptsPerCycle = 1,
                initialBackoffSeconds = 0.05, backoffCapSeconds = 0.05 },
            backend = new { kind = "alpaca", baseUrl = Url, deviceType = "focuser", deviceNumber = 19,
                connectionPolicy = "managed" }
        }));
        foreach (var number in numbers) config["outputs"]!.AsArray().Add(JsonSerializer.SerializeToNode(new {
            id = Guid.NewGuid(), number, label = "Private focuser " + number,
            device = new { kind = "proxy", source = SourceId, deviceType = "focuser" }
        }));
    }
    private async Task Serve()
    {
        try {
            while (!stopping.IsCancellationRequested) {
                var client = await listener.AcceptTcpClientAsync().ConfigureAwait(false);
                clients.TryAdd(client, 0); requests.Add(Handle(client));
            }
        } catch (Exception error) when (stopping.IsCancellationRequested && error is SocketException or ObjectDisposedException) { }
    }
    private async Task Handle(TcpClient client)
    {
        try {
            using var stream = client.GetStream();
            using var reader = new StreamReader(stream, Encoding.ASCII, false, 4096, true);
            var first = (await reader.ReadLineAsync().ConfigureAwait(false))!.Split(' ');
            var uri = new Uri("http://fixture" + first[1]);
            var length = 0;
            while (true) {
                var line = await reader.ReadLineAsync().ConfigureAwait(false);
                if (string.IsNullOrEmpty(line)) break;
                if (line.StartsWith("Content-Length:", StringComparison.OrdinalIgnoreCase)) length = int.Parse(line.Substring(15).Trim());
            }
            var chars = new char[length]; var offset = 0;
            while (offset < length) {
                var count = await reader.ReadAsync(chars, offset, length - offset).ConfigureAwait(false);
                if (count == 0) return; offset += count;
            }
            var text = first[0] == "PUT" ? new string(chars) : uri.Query.TrimStart('?');
            var args = text.Split('&').Where(part => part.Length > 0).Select(part => part.Split(new[] { '=' }, 2))
                .ToDictionary(pair => Uri.UnescapeDataString(pair[0]), pair => Uri.UnescapeDataString(pair[1]));
            if (!uri.AbsolutePath.StartsWith("/api/v1/focuser/19/", StringComparison.Ordinal) ||
                uint.Parse(args["ClientID"]) == 0 || uint.Parse(args["ClientTransactionID"]) == 0)
                throw new InvalidOperationException("Invalid private upstream request");
            var member = uri.Segments.Last(); object? value = null; var code = 0;
            if (first[0] == "PUT") {
                switch (member) {
                    case "connected": Volatile.Write(ref connected, bool.Parse(args["Connected"]) ? 1 : 0); break;
                    case "move":
                        Values["position"] = int.Parse(args["Position"]); Values["ismoving"] = true; Interlocked.Increment(ref moves);
                        if (LoseMoveReply) await Task.Delay(1000, stopping.Token).ConfigureAwait(false);
                        break;
                    case "halt": Interlocked.Increment(ref halts); Values["ismoving"] = false; break;
                    case "tempcomp": Values["tempcomp"] = bool.Parse(args["TempComp"]); break;
                    default: throw new InvalidOperationException("Unexpected private upstream write");
                }
            } else if (member == "connected") value = Volatile.Read(ref connected) != 0;
            else if (member == "interfaceversion") value = 3;
            else if (!Values.TryGetValue(member, out value)) code = 1024;
            var body = JsonSerializer.SerializeToUtf8Bytes(new { ErrorNumber = code, ErrorMessage = code == 0 ? "" : "private upstream detail", Value = value });
            var header = Encoding.ASCII.GetBytes("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: " + body.Length + "\r\nConnection: close\r\n\r\n");
            await stream.WriteAsync(header, 0, header.Length, stopping.Token).ConfigureAwait(false);
            await stream.WriteAsync(body, 0, body.Length, stopping.Token).ConfigureAwait(false);
        } catch (Exception error) when (error is IOException or SocketException or ObjectDisposedException or OperationCanceledException) { }
        finally { clients.TryRemove(client, out _); client.Dispose(); }
    }
    public void Dispose()
    {
        stopping.Cancel(); listener.Stop();
        serving.GetAwaiter().GetResult();
        foreach (var client in clients.Keys) client.Dispose();
        Task.WhenAll(requests).GetAwaiter().GetResult(); stopping.Dispose();
    }
}
