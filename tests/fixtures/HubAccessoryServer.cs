using System.Collections.Concurrent;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace Regain.TestFixtures;

internal static class HubFocuserSimulation
{
    internal static Guid AddTo(JsonObject config, params int[] numbers) => HubAccessorySimulation.AddTo(config, "focuser", numbers);
}
internal static class HubRotatorSimulation
{
    internal static Guid AddTo(JsonObject config, params int[] numbers) => HubAccessorySimulation.AddTo(config, "rotator", numbers);
}
internal static class HubFilterWheelSimulation
{
    internal static Guid AddTo(JsonObject config, params int[] numbers) => HubAccessorySimulation.AddTo(config, "filterwheel", numbers);
}
internal static class HubCoverCalibratorSimulation
{
    internal static Guid AddTo(JsonObject config, params int[] numbers) => HubAccessorySimulation.AddTo(config, "covercalibrator", numbers);
}
internal static class HubAccessorySimulation
{
    internal static Guid AddTo(JsonObject config, string type, params int[] numbers)
    {
        var source = Guid.NewGuid();
        config["sources"]!.AsArray().Add(JsonSerializer.SerializeToNode(new {
            id = source, label = "Explicit simulation " + type, backend = new { kind = "simulated", deviceType = type },
            polling = new { pollSeconds = 0.1, requestTimeoutSeconds = 0.3 } }));
        foreach (var number in numbers) config["outputs"]!.AsArray().Add(JsonSerializer.SerializeToNode(new {
            id = Guid.NewGuid(), number, label = "Simulation " + type + " " + number,
            device = new { kind = "proxy", source, deviceType = type } }));
        return source;
    }
}

// Private loopback-only upstream shared by net8 NINA and real net48 COM tests.
internal sealed class HubFocuserServer : HubAccessoryServer { internal HubFocuserServer() : base("focuser") { } }
internal sealed class HubRotatorServer : HubAccessoryServer { internal HubRotatorServer() : base("rotator") { } }
internal sealed class HubFilterWheelServer : HubAccessoryServer { internal HubFilterWheelServer() : base("filterwheel") { } }
internal sealed class HubCoverCalibratorServer : HubAccessoryServer { internal HubCoverCalibratorServer(int version = 2) : base("covercalibrator",version) { } }

internal class HubAccessoryServer : IDisposable
{
    private readonly string kind;
    private readonly int version;
    private readonly TcpListener listener = new(IPAddress.Loopback, 0);
    private readonly CancellationTokenSource stopping = new();
    private readonly ConcurrentDictionary<TcpClient, byte> clients = new();
    private readonly ConcurrentBag<Task> requests = new();
    private readonly ConcurrentQueue<string> trace = new();
    private readonly Task serving;
    internal readonly ConcurrentDictionary<string, object> Values = new();
    internal readonly ConcurrentQueue<string> PanelCommands = new();
    private int moves, halts, connected;
    internal int Moves => Volatile.Read(ref moves);
    internal int Halts => Volatile.Read(ref halts);
    internal string RequestTrace => string.Join("; ", trace);
    internal volatile bool LoseMoveReply;
    internal volatile bool IgnoreMove = false;
    internal string Url { get; }
    internal Guid SourceId { get; } = Guid.NewGuid();
    internal HubAccessoryServer(string kind,int version = 3)
    {
        this.kind = kind; this.version = version;
        if (kind == "covercalibrator") {
            Values["brightness"] = 0; Values["maxbrightness"] = 4096; Values["coverstate"] = 1;
            Values["calibratorstate"] = 1; Values["covermoving"] = false; Values["calibratorchanging"] = false;
        } else if (kind == "filterwheel") {
            Values["names"] = new[] {"L","Hα",""}; Values["focusoffsets"] = new[] {-12,0,17}; Values["position"] = 0;
        } else if (kind == "focuser") {
            Values["absolute"] = true; Values["maxstep"] = 1000; Values["maxincrement"] = 100;
            Values["tempcompavailable"] = true; Values["tempcomp"] = true; Values["position"] = 50;
            Values["temperature"] = -5.0;
        } else {
            Values["canreverse"] = true; Values["reverse"] = false; Values["position"] = 20.0;
            Values["mechanicalposition"] = 350.0; Values["targetposition"] = 20.0; Values["stepsize"] = 0.02;
        }
        Values["ismoving"] = false;
        listener.Start(); Url = "http://127.0.0.1:" + ((IPEndPoint)listener.LocalEndpoint).Port;
        serving = Serve();
    }
    internal void AddTo(JsonObject config, params uint[] numbers)
    {
        config["sources"]!.AsArray().Add(JsonSerializer.SerializeToNode(new {
            id = SourceId, label = "Private loopback " + kind,
            polling = new { pollSeconds = 0.1, requestTimeoutSeconds = 0.3, attemptsPerCycle = 1,
                initialBackoffSeconds = 0.05, backoffCapSeconds = 0.05 },
            backend = new { kind = "alpaca", baseUrl = Url, deviceType = kind, deviceNumber = 19,
                connectionPolicy = "managed" }
        }));
        foreach (var number in numbers) config["outputs"]!.AsArray().Add(JsonSerializer.SerializeToNode(new {
            id = Guid.NewGuid(), number, label = "Private " + kind + " " + number,
            device = new { kind = "proxy", source = SourceId, deviceType = kind }
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
        var operation = "unparsed request";
        var started = System.Diagnostics.Stopwatch.StartNew();
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
            if (!uri.AbsolutePath.StartsWith("/api/v1/" + kind + "/19/", StringComparison.Ordinal) ||
                uint.Parse(args["ClientID"]) == 0 || uint.Parse(args["ClientTransactionID"]) == 0)
                throw new InvalidOperationException("Invalid private upstream request");
            var member = uri.Segments.Last(); object? value = null; var code = 0;
            operation = first[0] + " " + member + " transaction=" + args["ClientTransactionID"];
            trace.Enqueue(operation + " started");
            if (first[0] == "PUT") {
                trace.Enqueue("write " + member + (args.TryGetValue("Position", out var position) ? " position=" + position : ""));
                switch (member) {
                    case "connected": Volatile.Write(ref connected, bool.Parse(args["Connected"]) ? 1 : 0); break;
                    case "connect" when kind == "filterwheel" || kind == "covercalibrator" && version >= 2: Volatile.Write(ref connected,1); break;
                    case "disconnect" when kind == "filterwheel" || kind == "covercalibrator" && version >= 2: Volatile.Write(ref connected,0); break;
                    case "opencover": case "closecover": case "haltcover": case "calibratoron": case "calibratoroff":
                        if (kind != "covercalibrator" || args.Count != (member == "calibratoron" ? 3 : 2)) throw new InvalidOperationException("Invalid panel command");
                        PanelCommands.Enqueue(member);
                        if (member == "opencover" || member == "closecover") { Values["coverstate"] = 2; Values["covermoving"] = true; }
                        else if (member == "haltcover") { Values["coverstate"] = 4; Values["covermoving"] = false; }
                        else if (member == "calibratoron") { Values["brightness"] = int.Parse(args["Brightness"]); Values["calibratorstate"] = 2; Values["calibratorchanging"] = true; }
                        else { Values["brightness"] = 0; Values["calibratorstate"] = 1; Values["calibratorchanging"] = false; }
                        if (LoseMoveReply) await Task.Delay(1000,stopping.Token).ConfigureAwait(false);
                        break;
                    case "position" when kind == "filterwheel":
                        Values["position"] = -1; Interlocked.Increment(ref moves);
                        if (LoseMoveReply) await Task.Delay(1000,stopping.Token).ConfigureAwait(false);
                        break;
                    case "move" when kind == "focuser":
                        Values["position"] = int.Parse(args["Position"]); Values["ismoving"] = true; Interlocked.Increment(ref moves);
                        if (LoseMoveReply) await Task.Delay(1000, stopping.Token).ConfigureAwait(false);
                        break;
                    case "move": case "moveabsolute": case "movemechanical":
                        if (IgnoreMove) { Interlocked.Increment(ref moves); break; }
                        var angle = double.Parse(args["Position"], System.Globalization.CultureInfo.InvariantCulture);
                        var logical = Convert.ToDouble(Values["position"]); var mechanical = Convert.ToDouble(Values["mechanicalposition"]);
                        var target = member == "move" ? logical + angle : member == "movemechanical" ? angle + logical - mechanical : angle;
                        target = (target % 360 + 360) % 360;
                        Values["mechanicalposition"] = ((mechanical + target - logical) % 360 + 360) % 360;
                        Values["position"] = target; Values["targetposition"] = target; Values["ismoving"] = true;
                        Interlocked.Increment(ref moves);
                        if (LoseMoveReply) await Task.Delay(1000, stopping.Token).ConfigureAwait(false);
                        break;
                    case "sync":
                        Values["position"] = double.Parse(args["Position"], System.Globalization.CultureInfo.InvariantCulture);
                        Values["targetposition"] = Values["position"]; break;
                    case "reverse": Values["reverse"] = bool.Parse(args["Reverse"]); break;
                    case "halt": Interlocked.Increment(ref halts); Values["ismoving"] = false; break;
                    case "tempcomp": Values["tempcomp"] = bool.Parse(args["TempComp"]); break;
                    default: throw new InvalidOperationException("Unexpected private upstream write");
                }
            } else if (member == "connected") value = Volatile.Read(ref connected) != 0;
            else if (member == "connecting" && (kind == "filterwheel" || kind == "covercalibrator" && version >= 2)) value = false;
            else if (member == "interfaceversion") value = version;
            else if (kind == "covercalibrator" && version == 1 && member is "covermoving" or "calibratorchanging") code = 1024;
            else if (!Values.TryGetValue(member, out value)) code = 1024;
            var body = JsonSerializer.SerializeToUtf8Bytes(new { ErrorNumber = code, ErrorMessage = code == 0 ? "" : "private upstream detail", Value = value });
            var header = Encoding.ASCII.GetBytes("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: " + body.Length + "\r\nConnection: close\r\n\r\n");
            await stream.WriteAsync(header, 0, header.Length, stopping.Token).ConfigureAwait(false);
            await stream.WriteAsync(body, 0, body.Length, stopping.Token).ConfigureAwait(false);
            trace.Enqueue(operation + " replied code=" + code + " elapsedMs=" + started.ElapsedMilliseconds);
        } catch (Exception error) when (error is IOException or SocketException or ObjectDisposedException or OperationCanceledException) {
            trace.Enqueue(operation + " failed " + error);
        }
        finally {
            trace.Enqueue(operation + " closed elapsedMs=" + started.ElapsedMilliseconds);
            clients.TryRemove(client, out _); client.Dispose();
        }
    }
    public void Dispose()
    {
        stopping.Cancel(); listener.Stop();
        serving.GetAwaiter().GetResult();
        foreach (var client in clients.Keys) client.Dispose();
        Task.WhenAll(requests).GetAwaiter().GetResult(); stopping.Dispose();
    }
}
