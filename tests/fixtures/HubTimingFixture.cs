using System.IO;
using System.IO.Pipes;
using System.Text.Json;
using System.Text.Json.Nodes;
using Regain.Hub;

namespace Regain.TestFixtures;

internal static class HubTimingFixture
{
    private static void Check(bool value, string message) { if (!value) throw new InvalidOperationException(message); }
    internal static readonly string[] Faults = ["host", "revision", "client", "output", "source", "zero", "negative", "large", "fraction", "missing", "unknown"];
    internal static readonly string[] CaptureFaults = Faults.Concat(new[] { "duration", "readiness", "completion", "native" }).ToArray();
    internal static async Task RunAll()
    {
        await Semantics(); foreach (var fault in Faults) await Malformed(fault);
        await CaptureSemantics(); foreach (var fault in CaptureFaults) await MalformedCapture(fault);
        Console.WriteLine($"Camera timing {IntPtr.Size * 8}-bit: bounded negotiation, identity/shape rejection, timed acknowledgement and unchanged scalar deadline passed");
    }
    internal static async Task CaptureSemantics()
    {
        using var peer = await Peer.Open();
        foreach (var seconds in new[] { double.NaN, double.PositiveInfinity, -1.0 }) {
            try { await peer.Client.GetCameraCaptureTimingAsync(peer.Output, seconds); throw new Exception("Invalid duration was queried"); }
            catch (HubException error) { Check(error.Failure == HubFailure.InvalidRequest, "Invalid capture duration classification changed"); }
        }
        var serving = peer.ServeCaptureTiming();
        var value = await peer.Client.GetCameraCaptureTimingAsync(peer.Output, 600.0); await serving;
        Check(value.DurationSeconds == 600.0 && value.Readiness == TimeSpan.FromSeconds(630) && value.Completion == TimeSpan.FromSeconds(995)
            && value.Source == peer.Source && value.Native, "Capture timing lost duration, bounds or identity");
        Check(peer.Client.IsConnected, "Inert timing query retired control");
    }
    internal static async Task MalformedCapture(string fault)
    {
        using var peer = await Peer.Open();
        var serving = peer.ServeCaptureTiming(fault);
        try { await peer.Client.GetCameraCaptureTimingAsync(peer.Output, 600.0); throw new Exception("Malformed capture timing was admitted"); }
        catch (HubException error) { Check(error.Failure == HubFailure.Protocol, $"Malformed capture timing error changed ({fault}): {error}"); }
        await serving; Check(peer.Client.IsConnected, "Malformed capture metadata retired another connection");
    }
    internal static async Task Malformed(string fault)
    {
        using var peer = await Peer.Open();
        var serving = peer.ServeTiming(fault);
        try { await peer.Client.GetCameraTimingAsync(peer.Output); throw new InvalidOperationException("Malformed timing admitted"); }
        catch (HubException error) { Check(error.Failure == HubFailure.Protocol, $"Malformed timing error changed ({fault}): {error}"); }
        await serving; Check(peer.Client.IsConnected, "Invalid descriptor retired another connection");
    }
    internal static async Task Semantics()
    {
        using var peer = await Peer.Open();
        var serving = peer.ServeTiming(); var timing = await peer.Client.GetCameraTimingAsync(peer.Output); await serving;
        Check(timing.Source == peer.Source && timing.Connect == TimeSpan.FromSeconds(10), "Negotiated timing changed");
        foreach (var command in new object[] {
            new { op = "get", output = peer.Output, property = new { member = "cameraAcquisition" } },
            new { op = "connect", output = Guid.NewGuid() },
            new { op = "changeConnection", output = peer.Output, connected = true, asynchronous = true },
            new { op = "put", output = peer.Output, property = new { member = "setSwitch", id = 0, state = true } } }) {
            try { await peer.Client.RequestCameraAsync(timing, JsonSerializer.SerializeToElement(command)); throw new InvalidOperationException("Non-camera operation extended"); }
            catch (HubException error) { Check(error.Failure == HubFailure.InvalidRequest, "Invalid timing target error changed"); }
        }
        var incoming = peer.Read();
        var write = peer.Client.RequestCameraAsync(timing, JsonSerializer.SerializeToElement(new { op = "put", output = peer.Output,
            property = new { member = "cameraSetting", setting = new { property = "numX", value = 64 } } }));
        var request = await incoming; Check(request.GetProperty("id").GetInt32() == 3, "Rejected local commands consumed request IDs");
        var wrapper = request.GetProperty("command");
        Check(wrapper.GetProperty("op").GetString() == "cameraControl" && wrapper.GetProperty("expectedRevision").GetGuid() == timing.ConfigurationRevision &&
            wrapper.GetProperty("command").GetProperty("op").GetString() == "put", "Camera mutation lost its revision fence");
        // The ordinary client deadline is two seconds. Only this acknowledged
        // camera operation has a ten-second negotiated server allowance.
        await Task.Delay(2200); Check(!write.IsCompleted && peer.Client.IsConnected, "Camera command used the short scalar deadline");
        await peer.Reply(3, null); await write;
        incoming = peer.Read();
        write = peer.Client.RequestCameraAsync(timing, JsonSerializer.SerializeToElement(new { op = "put", output = peer.Output,
            property = new { member = "pulseGuide", request = new { direction = 2, durationMilliseconds = 100 } } }));
        request = await incoming; wrapper = request.GetProperty("command");
        Check(wrapper.GetProperty("op").GetString() == "cameraControl" && wrapper.GetProperty("expectedRevision").GetGuid() == timing.ConfigurationRevision &&
            wrapper.GetProperty("command").GetProperty("property").GetProperty("request").GetProperty("durationMilliseconds").GetInt32() == 100,
            "Pulse guide lost its typed duration or revision fence");
        await peer.Reply(request.GetProperty("id").GetInt32(), Guid.NewGuid()); await write;
        incoming = peer.Read();
        using var cancel = new CancellationTokenSource();
        write = peer.Client.RequestCameraAsync(timing, JsonSerializer.SerializeToElement(new { op = "connect", output = peer.Output }), cancel.Token);
        request = await incoming; cancel.Cancel();
        try { await write; throw new InvalidOperationException("Cancelled camera waiter returned success"); } catch (OperationCanceledException) { }
        Check(peer.Client.IsConnected, "Camera cancellation closed control transport");
        await peer.Reply(request.GetProperty("id").GetInt32(), null);
        incoming = peer.Read();
        var read = peer.Client.RequestAsync(JsonSerializer.SerializeToElement(new { op = "get", output = peer.Output, property = new { member = "connected" } }));
        await incoming;
        try { await read; throw new InvalidOperationException("Scalar read deadline extended"); }
        catch (HubException error) { Check(error.Failure == HubFailure.Timeout, "Scalar read lost its original deadline"); }
    }
    private sealed class Peer : IDisposable
    {
        private readonly NamedPipeServerStream server;
        private readonly NamedPipeClientStream stream;
        private readonly Guid instance = Guid.NewGuid(), host = Guid.NewGuid(), revision = Guid.NewGuid(), client = Guid.NewGuid();
        internal readonly Guid Output = Guid.NewGuid(), Source = Guid.NewGuid();
        internal HubClient Client = null!;
        private Peer(string name)
        {
            server = new NamedPipeServerStream(name, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous, 8192, 8192);
            stream = new NamedPipeClientStream(".", name, PipeDirection.InOut, PipeOptions.Asynchronous);
        }
        internal static async Task<Peer> Open()
        {
            var peer = new Peer("Regain.Timing.Fixture." + Guid.NewGuid().ToString("N"));
            try {
                var accepting = peer.server.WaitForConnectionAsync(); await peer.stream.ConnectAsync(10000); await accepting;
                async Task ServeHello() {
                    var request = await peer.Read(); Check(request.GetProperty("id").GetInt32() == 1, "Missing timing hello");
                    await peer.Reply(1, new { protocolVersion = 1, instanceId = peer.instance, hostInstance = peer.host,
                        configurationRevision = peer.revision, clientId = peer.client, maxFrameBytes = 1048576, maxInFlight = 2,
                        operations = new[] { "cameraTiming", "cameraCaptureTiming", "cameraControl", "get", "put", "connect", "changeConnection" }, capabilities = new[] { "cameraOperationTiming", "cameraCaptureTiming" } });
                }
                var serving = ServeHello();
                peer.Client = await HubClient.FromStreamAsync(peer.stream, peer.instance, TimeSpan.FromSeconds(10),
                    new HubClientLimits(TimeSpan.FromSeconds(1), TimeSpan.FromSeconds(2)));
                await serving; return peer;
            } catch { peer.Dispose(); throw; }
        }
        internal async Task<JsonElement> Read()
        {
            using var stop = new CancellationTokenSource(TimeSpan.FromSeconds(10));
            return HubWire.Parse(await HubWire.ReadFrame(server, HubWire.MaxFrame, TimeSpan.FromSeconds(5), stop.Token));
        }
        internal Task Reply(int id, object? result) => HubWire.WriteFrame(server,
            JsonSerializer.SerializeToUtf8Bytes(new { version = 1, id, result }), TimeSpan.FromSeconds(5), CancellationToken.None);
        internal async Task ServeTiming(string? fault = null)
        {
            var request = await Read(); var command = request.GetProperty("command");
            Check(command.GetProperty("op").GetString() == "cameraTiming" && command.GetProperty("output").GetGuid() == Output &&
                command.GetProperty("expectedRevision").GetGuid() == revision, "Timing query lost its identity/revision fence");
            var value = new JsonObject { ["hostInstance"] = host.ToString(), ["configurationRevision"] = revision.ToString(),
                ["clientId"] = client.ToString(), ["output"] = Output.ToString(), ["source"] = Source.ToString(), ["native"] = true,
                ["connectMilliseconds"] = 10000, ["startMilliseconds"] = 10000, ["settingMilliseconds"] = 10000, ["stopMilliseconds"] = 10000, ["abortMilliseconds"] = 10000 };
            var key = fault switch { "host" => "hostInstance", "revision" => "configurationRevision", "client" => "clientId", "output" => "output", "source" => "source", _ => null };
            if (key is not null) value[key] = (fault == "source" ? Guid.Empty : Guid.NewGuid()).ToString();
            switch (fault) {
                case "zero": value["connectMilliseconds"] = 0; break;
                case "negative": value["startMilliseconds"] = -1; break;
                case "large": value["settingMilliseconds"] = HubCameraTiming.MaximumMilliseconds + 1; break;
                case "fraction": value["stopMilliseconds"] = 1.5; break;
                case "missing": value.Remove("abortMilliseconds"); break;
                case "unknown": value["extra"] = 1; break;
            }
            await Reply(request.GetProperty("id").GetInt32(), value);
        }
        internal async Task ServeCaptureTiming(string? fault = null)
        {
            var request = await Read(); var command = request.GetProperty("command");
            Check(request.GetProperty("id").GetInt32() == 2 && command.GetProperty("op").GetString() == "cameraCaptureTiming"
                && command.GetProperty("output").GetGuid() == Output && command.GetProperty("expectedRevision").GetGuid() == revision
                && command.GetProperty("durationSeconds").GetDouble() == 600.0, "Capture query lost identity/revision/duration or invalid queries consumed IDs");
            var value = new JsonObject { ["hostInstance"] = host.ToString(), ["configurationRevision"] = revision.ToString(),
                ["clientId"] = client.ToString(), ["output"] = Output.ToString(), ["source"] = Source.ToString(), ["native"] = true,
                ["durationSeconds"] = 600.0, ["readinessMilliseconds"] = 630000, ["completionMilliseconds"] = 995000 };
            var key = fault switch { "host" => "hostInstance", "revision" => "configurationRevision", "client" => "clientId", "output" => "output", "source" => "source", _ => null };
            if (key is not null) value[key] = (fault == "source" ? Guid.Empty : Guid.NewGuid()).ToString();
            switch (fault) {
                case "zero": value["readinessMilliseconds"] = 0; break;
                case "negative": value["completionMilliseconds"] = -1; break;
                case "large": value["completionMilliseconds"] = HubCameraTiming.MaximumMilliseconds + 1; break;
                case "fraction": value["completionMilliseconds"] = 1.5; break;
                case "missing": value.Remove("completionMilliseconds"); break;
                case "unknown": value["extra"] = 1; break;
                case "duration": value["durationSeconds"] = 601.0; break;
                case "readiness": value["readinessMilliseconds"] = 599999; break;
                case "completion": value["completionMilliseconds"] = 629999; break;
                case "native": value["native"] = "true"; break;
            }
            await Reply(request.GetProperty("id").GetInt32(), value);
        }
        public void Dispose() { Client?.Dispose(); stream.Dispose(); server.Dispose(); }
    }
}
