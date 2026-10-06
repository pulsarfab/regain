using System.Diagnostics;
using System.IO.Pipes;
using System.Security.AccessControl;
using System.Security.Principal;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using Regain.Hub;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class HubClientTests
{
    private static JsonElement Command(string op = "getConfig") => JsonSerializer.SerializeToElement(new { op });
    private static async Task<HubException> Fails(HubFailure failure, Func<Task> action)
    {
        var error = await Assert.ThrowsAsync<HubException>(action);
        Assert.Equal(failure, error.Failure); return error;
    }

    private sealed class Peer : IDisposable
    {
        private readonly NamedPipeServerStream server;
        private readonly NamedPipeClientStream stream;
        private byte? firstByte;
        internal HubClient Client = null!;
        internal Guid Instance = Guid.NewGuid();
        private Peer(string name)
        {
            server = new NamedPipeServerStream(name, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous, 256, 256);
            stream = new NamedPipeClientStream(".", name, PipeDirection.InOut, PipeOptions.Asynchronous);
        }
        internal static async Task<Peer> Open(Action<JsonObject>? alter = null, HubClientLimits? limits = null)
        {
            var peer = new Peer("Regain.Hub.Tests." + Guid.NewGuid().ToString("N"));
            try {
                var accepting = peer.server.WaitForConnectionAsync();
                await peer.stream.ConnectAsync(2000);
                await accepting.WaitAsync(TimeSpan.FromSeconds(2));
                var serving = Task.Run(async () => {
                    var request = await peer.Read();
                    Assert.Equal(1ul, request.GetProperty("id").GetUInt64());
                    Assert.Equal("hello", request.GetProperty("command").GetProperty("op").GetString());
                    var hello = new JsonObject {
                        ["protocolVersion"] = 1, ["instanceId"] = peer.Instance.ToString(), ["hostInstance"] = Guid.NewGuid().ToString(),
                        ["configurationRevision"] = Guid.NewGuid().ToString(), ["clientId"] = Guid.NewGuid().ToString(),
                        ["maxFrameBytes"] = 1048576, ["maxInFlight"] = 2,
                        ["operations"] = new JsonArray("getConfig", "put", "changeConnection", "futureOperation"),
                        ["capabilities"] = new JsonArray("scalarDeviceState", "asyncOutputConnection")
                    };
                    alter?.Invoke(hello);
                    await peer.Raw(JsonSerializer.SerializeToUtf8Bytes(new { version = 1, id = 1, result = hello }));
                });
                peer.Client = await HubClient.FromStreamAsync(peer.stream, peer.Instance, TimeSpan.FromSeconds(2), limits);
                await serving; return peer;
            } catch { peer.Dispose(); throw; }
        }
        internal Task<JsonElement> Read() => ReadCore();
        private async Task<JsonElement> ReadCore()
        {
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(3));
            if (firstByte is byte prefix) {
                firstByte = null;
                byte[] header = [prefix, 0, 0, 0];
                await server.ReadExactlyAsync(header.AsMemory(1), deadline.Token);
                var size = System.Buffers.Binary.BinaryPrimitives.ReadUInt32LittleEndian(header);
                Assert.InRange(size, 1u, (uint)HubWire.MaxFrame);
                var bytes = new byte[(int)size];
                await server.ReadExactlyAsync(bytes, deadline.Token);
                return HubWire.Parse(bytes);
            }
            return HubWire.Parse(await HubWire.ReadFrame(server, HubWire.MaxFrame, TimeSpan.FromSeconds(2), deadline.Token));
        }
        internal async Task Raw(byte[] bytes) => await HubWire.WriteFrame(server, bytes, TimeSpan.FromSeconds(2), CancellationToken.None);
        internal Task Reply(ulong id, object? result) => Raw(JsonSerializer.SerializeToUtf8Bytes(new { version = 1, id, result }));
        internal async Task Partial() { await server.WriteAsync(new byte[] { 42 }); await server.FlushAsync(); }
        internal async Task Header(uint length)
        {
            var bytes = new byte[4];
            System.Buffers.Binary.BinaryPrimitives.WriteUInt32LittleEndian(bytes, length);
            await server.WriteAsync(bytes); await server.FlushAsync();
        }
        internal async Task PartialRead()
        {
            var first = new byte[1];
            Assert.Equal(1, await server.ReadAsync(first).AsTask().WaitAsync(TimeSpan.FromSeconds(2)));
            firstByte = first[0];
        }
        internal void End() => server.Dispose();
        public void Dispose() { Client?.Dispose(); stream.Dispose(); server.Dispose(); }
    }

    [Fact]
    public async Task OutOfOrderRepliesAndNullResultsStayCorrelated()
    {
        using var peer = await Peer.Open();
        var first = peer.Client.RequestAsync(Command());
        var second = peer.Client.RequestAsync(Command());
        var a = await peer.Read(); var b = await peer.Read();
        Assert.True(a.GetProperty("id").GetUInt64() < b.GetProperty("id").GetUInt64());
        await peer.Reply(b.GetProperty("id").GetUInt64(), new { sample = 42 });
        Assert.Equal(42, (await second).GetProperty("sample").GetInt32());
        await peer.Reply(a.GetProperty("id").GetUInt64(), null);
        Assert.Equal(JsonValueKind.Null, (await first).ValueKind);
    }

    [Fact]
    public async Task CancelledDispatchedRequestsKeepCapacityUntilReplyAndAreNotReplayed()
    {
        using var peer = await Peer.Open();
        using var token = new CancellationTokenSource();
        var first = peer.Client.RequestAsync(Command("put"), token.Token);
        var a = await peer.Read();
        token.Cancel(); await Assert.ThrowsAnyAsync<OperationCanceledException>(() => first);
        var second = peer.Client.RequestAsync(Command()); var b = await peer.Read();
        await Fails(HubFailure.Busy, () => peer.Client.RequestAsync(Command()));
        await peer.Reply(a.GetProperty("id").GetUInt64(), null);
        await peer.Reply(b.GetProperty("id").GetUInt64(), 7);
        Assert.Equal(7, (await second).GetInt32());
        var third = peer.Client.RequestAsync(Command()); var c = await peer.Read();
        Assert.Equal(4ul, c.GetProperty("id").GetUInt64());
        await peer.Reply(4, true); Assert.True((await third).GetBoolean());
    }

    [Fact]
    public async Task QueuedCancellationReturnsCapacityWithoutSendingItsCommand()
    {
        using var peer = await Peer.Open(limits: new HubClientLimits(TimeSpan.FromSeconds(2), TimeSpan.FromSeconds(5)));
        var first = peer.Client.RequestAsync(JsonSerializer.SerializeToElement(new { op = "put", payload = new string('x', 100000) }));
        await peer.PartialRead();
        using var token = new CancellationTokenSource();
        var queued = peer.Client.RequestAsync(Command("put"), token.Token);
        token.Cancel(); await Assert.ThrowsAnyAsync<OperationCanceledException>(() => queued);
        using var borrowed = JsonDocument.Parse("{\"op\":\"getConfig\"}");
        var next = peer.Client.RequestAsync(borrowed.RootElement);
        borrowed.Dispose();
        var a = await peer.Read(); await peer.Reply(a.GetProperty("id").GetUInt64(), null); await first;
        var b = await peer.Read();
        Assert.Equal("getConfig", b.GetProperty("command").GetProperty("op").GetString());
        Assert.Equal(3ul, b.GetProperty("id").GetUInt64());
        await peer.Reply(3, true); Assert.True((await next).GetBoolean());
    }

    [Fact]
    public async Task TransportLossMakesDispatchedWritesUncertainAndTheClientTerminal()
    {
        using var peer = await Peer.Open();
        var write = peer.Client.RequestAsync(Command("changeConnection")); await peer.Read();
        var read = peer.Client.RequestAsync(Command()); await peer.Read();
        peer.End();
        await Fails(HubFailure.Uncertain, () => write);
        await Fails(HubFailure.Disconnected, () => read);
        await peer.Client.Closed.WaitAsync(TimeSpan.FromSeconds(2));
        Assert.False(peer.Client.IsConnected);
        await Fails(HubFailure.Disconnected, () => peer.Client.RequestAsync(Command()));
    }

    [Fact]
    public async Task DisposingClientClosesItsOwnTransportAndRetainsUncertainWrites()
    {
        using var peer = await Peer.Open();
        var write = peer.Client.RequestAsync(Command("put")); await peer.Read();
        peer.Client.Dispose(); await Fails(HubFailure.Uncertain, () => write);
        Assert.Equal(0, await peerServerReadEof(peer));
    }
    private static async Task<int> peerServerReadEof(Peer peer)
    {
        try { await peer.Read(); return 1; }
        catch (HubException e) when (e.Failure == HubFailure.Disconnected) { return 0; }
    }

    [Fact]
    public async Task AbandonedPublicClientCanBeCollectedWhileItsReaderIsIdle()
    {
        using var peer = await Peer.Open();
        var (reference, closed) = Abandon(peer);
        for (var i = 0; i < 10 && !closed.IsCompleted; i++) {
            GC.Collect(); GC.WaitForPendingFinalizers(); await Task.Delay(10);
        }
        Assert.False(reference.IsAlive);
        await closed.WaitAsync(TimeSpan.FromSeconds(2));
        Assert.Equal(0, await peerServerReadEof(peer));
    }
    [System.Runtime.CompilerServices.MethodImpl(System.Runtime.CompilerServices.MethodImplOptions.NoInlining)]
    private static (WeakReference, Task) Abandon(Peer peer)
    {
        var result = (new WeakReference(peer.Client), peer.Client.Closed);
        peer.Client = null!; return result;
    }

    [Theory]
    [InlineData("instanceId")]
    [InlineData("hostInstance")]
    [InlineData("configurationRevision")]
    [InlineData("clientId")]
    [InlineData("protocolVersion")]
    [InlineData("maxFrameBytes")]
    [InlineData("maxInFlight")]
    [InlineData("unknown")]
    public async Task InvalidHelloIdentityAndLimitsAreRejected(string member)
    {
        await Fails(HubFailure.Protocol, async () => {
            using var peer = await Peer.Open(hello => {
                if (member.EndsWith("Id") || member is "hostInstance" or "configurationRevision") hello[member] = Guid.Empty.ToString();
                else hello[member] = member == "maxFrameBytes" ? 1048577 : member == "maxInFlight" ? 9 : 2;
            });
        });
    }

    [Theory]
    [InlineData("{\"version\":1,\"version\":1,\"id\":2,\"result\":null}")]
    [InlineData("{\"version\":1,\"id\":2,\"result\":null,\"error\":{\"code\":\"busy\",\"message\":\"safe\"}}")]
    [InlineData("{\"version\":1,\"id\":2}")]
    [InlineData("{\"version\":1,\"id\":99,\"result\":null}")]
    [InlineData("{\"version\":1,\"id\":2,\"result\":{\"value\":1,\"value\":2}}")]
    [InlineData("{\"version\":1,\"id\":2,\"result\":null,\"unexpected\":1}")]
    public async Task MalformedUnknownAndDuplicateRepliesCloseWithoutReplaying(string json)
    {
        using var peer = await Peer.Open();
        var request = peer.Client.RequestAsync(Command()); await peer.Read();
        await peer.Raw(Encoding.UTF8.GetBytes(json));
        await Fails(HubFailure.Protocol, () => request);
        await peer.Client.Closed.WaitAsync(TimeSpan.FromSeconds(2));
    }

    [Fact]
    public async Task RemoteErrorsRetainStructuredFieldsWithoutPuttingRawTextInExceptions()
    {
        using var peer = await Peer.Open();
        var request = peer.Client.RequestAsync(Command()); await peer.Read();
        await peer.Raw(JsonSerializer.SerializeToUtf8Bytes(new { version = 1, id = 2,
            error = new { code = "validation", message = "private-driver-detail", upstreamCode = 1031, retryAfterSeconds = 3.5,
                fields = new[] { new { path = "outputs.0.label", code = "length", message = "Label is empty" } } } }));
        var error = await Fails(HubFailure.Remote, () => request);
        Assert.Equal("validation", error.Remote!.Code);
        Assert.Equal(1031, error.Remote.UpstreamCode);
        Assert.Equal(3.5, error.Remote.RetryAfterSeconds);
        Assert.Equal("outputs.0.label", error.Remote.Fields[0].GetProperty("path").GetString());
        Assert.DoesNotContain("private-driver-detail", error.ToString());
        Assert.True(peer.Client.IsConnected);
    }

    [Fact]
    public async Task InvalidAndOversizedRequestsAreRejectedBeforeWritingAndClientStaysUsable()
    {
        using var peer = await Peer.Open();
        await Fails(HubFailure.InvalidRequest, () => peer.Client.RequestAsync(Command("hello")));
        await Fails(HubFailure.InvalidRequest, () => peer.Client.RequestAsync(Command("futureOperation")));
        await Fails(HubFailure.InvalidRequest, () => peer.Client.RequestAsync(JsonSerializer.SerializeToElement(new { op = "put", secret = new string('x', 1048576) })));
        using var duplicate = JsonDocument.Parse("{\"op\":\"getConfig\",\"op\":\"put\"}");
        await Fails(HubFailure.InvalidRequest, () => peer.Client.RequestAsync(duplicate.RootElement));
        using var cancelled = new CancellationTokenSource(); cancelled.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => peer.Client.RequestAsync(Command(), cancelled.Token));
        var next = peer.Client.RequestAsync(Command());
        Assert.Equal(2ul, (await peer.Read()).GetProperty("id").GetUInt64());
        await peer.Reply(2, true); Assert.True((await next).GetBoolean());
    }

    [Fact]
    public async Task NegotiatedSmallerFramesAreEnforcedBeforeDispatch()
    {
        using var peer = await Peer.Open(hello => hello["maxFrameBytes"] = 256);
        Assert.Equal(256, peer.Client.Hello.MaxFrameBytes);
        await Fails(HubFailure.InvalidRequest, () => peer.Client.RequestAsync(JsonSerializer.SerializeToElement(new { op = "put", payload = new string('\u03bb', 100) })));
        var next = peer.Client.RequestAsync(Command());
        Assert.Equal(2ul, (await peer.Read()).GetProperty("id").GetUInt64());
        await peer.Reply(2, null); Assert.Equal(JsonValueKind.Null, (await next).ValueKind);
    }

    [Theory]
    [InlineData(0u)]
    [InlineData(1048577u)]
    [InlineData(uint.MaxValue)]
    public async Task InvalidFrameLengthsFailBeforeAllocatingPayload(uint length)
    {
        using var peer = await Peer.Open();
        var request = peer.Client.RequestAsync(Command()); await peer.Read();
        await peer.Header(length); await Fails(HubFailure.Protocol, () => request);
    }

    [Fact]
    public async Task IdleStreamsStayOpenButPartialFramesHaveAnIndependentDeadline()
    {
        using var peer = await Peer.Open(limits: new HubClientLimits(TimeSpan.FromMilliseconds(200), TimeSpan.FromSeconds(3)));
        await Task.Delay(450); Assert.True(peer.Client.IsConnected);
        var request = peer.Client.RequestAsync(Command()); await peer.Read();
        await peer.Partial(); await Fails(HubFailure.Timeout, () => request);
    }

    [Fact]
    public async Task DispatchedRequestDeadlineRetainsUncertaintyAndDoesNotReplay()
    {
        using var peer = await Peer.Open(limits: new HubClientLimits(TimeSpan.FromSeconds(1), TimeSpan.FromMilliseconds(200)));
        var write = peer.Client.RequestAsync(Command("put"));
        Assert.Equal(2ul, (await peer.Read()).GetProperty("id").GetUInt64());
        await Fails(HubFailure.Uncertain, () => write);
        Assert.False(peer.Client.IsConnected);
    }

    [Fact]
    public async Task StalledWriteClosesAtItsFrameDeadlineEvenAfterCallerCancellation()
    {
        using var peer = await Peer.Open(limits: new HubClientLimits(TimeSpan.FromMilliseconds(200), TimeSpan.FromSeconds(3)));
        using var token = new CancellationTokenSource();
        var write = peer.Client.RequestAsync(JsonSerializer.SerializeToElement(new { op = "put", payload = new string('x', 100000) }), token.Token);
        // Read only the first length byte, so a large write cannot drain.
        await peer.PartialRead(); token.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => write);
        await peer.Client.Closed.WaitAsync(TimeSpan.FromSeconds(2));
        Assert.False(peer.Client.IsConnected);
    }

    [Fact]
    public void PipeDescriptorRequiresAnExactProtectedUserOwnerAndSingleAllowAce()
    {
        using var identity = WindowsIdentity.GetCurrent(); var sid = identity.User!;
        HubPipe.VerifyDescriptor(new RawSecurityDescriptor($"O:{sid}D:P(A;OICI;GA;;;{sid})"), sid);
        foreach (var text in new[] {
            $"O:{sid}D:(A;;GA;;;{sid})", $"O:{sid}D:P(A;;GA;;;WD)",
            $"O:{sid}D:P(A;;GA;;;{sid})(A;;GA;;;SY)", $"O:SYD:P(A;;GA;;;{sid})", $"O:{sid}D:P(D;;GA;;;{sid})"
        }) Assert.Throws<HubException>(() => HubPipe.VerifyDescriptor(new RawSecurityDescriptor(text), sid));
        Assert.Throws<HubException>(() => HubPipe.Name(@"\\host\pipe\PulsarFab.Regain.Hub." + new string('a', 64)));
        Assert.Throws<HubException>(() => HubPipe.Name(@"\\.\pipe\unrelated"));
        Assert.Equal("\"C:\\Unicode \u03bb\\path\\\\\"", HubAttachment.Quote("C:\\Unicode \u03bb\\path\\"));
        Assert.Equal("\"a\\\"b\"", HubAttachment.Quote("a\"b"));
        Assert.True(HubAttachment.FullyQualified(@"C:\local\config.json"));
        Assert.True(HubAttachment.FullyQualified(@"\\server\share\config.json"));
        Assert.False(HubAttachment.FullyQualified(@"C:config.json"));
        Assert.False(HubAttachment.FullyQualified(@"\config.json"));
    }

    [Fact]
    public async Task ProductionConnectorRejectsAPermissivePipeBeforeSendingHello()
    {
        var name = "PulsarFab.Regain.Hub." + Guid.NewGuid().ToString("N") + Guid.NewGuid().ToString("N");
        using var server = new NamedPipeServerStream(name, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous);
        var accepting = server.WaitForConnectionAsync();
        var instance = Guid.NewGuid();
        var attachment = new HubAttachment(JsonSerializer.SerializeToElement(new { protocolVersion = 1, instanceId = instance,
            hostInstance = Guid.NewGuid(), configurationRevision = Guid.NewGuid(), transport = "namedPipe", address = @"\\.\pipe\" + name }), instance);
        await Fails(HubFailure.Protocol, () => HubClient.ConnectAsync(attachment, TimeSpan.FromSeconds(2)));
        await accepting.WaitAsync(TimeSpan.FromSeconds(2));
        Assert.Equal(0, await server.ReadAsync(new byte[1]).AsTask().WaitAsync(TimeSpan.FromSeconds(2)));
    }

    [Fact]
    public async Task NativeAttachmentSharesOneRealHostAndDisconnectsOnlyItsOwnLeases()
    {
        var root = Repository();
        var directory = Path.Combine(Path.GetTempPath(), "Regain native hub \u03bb " + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        var path = Path.Combine(directory, "configuration.json");
        var config = JsonNode.Parse(File.ReadAllText(Path.Combine(root, "crates", "regain-hub", "examples", "simulated-observatory.json")))!.AsObject();
        config["instanceId"] = Guid.NewGuid().ToString(); config["revision"] = Guid.NewGuid().ToString();
        await File.WriteAllTextAsync(path, config.ToJsonString());
        var workerDirectory = Environment.GetEnvironmentVariable("REGAIN_TEST_WORKERS") ?? Path.Combine(root, "target", "debug");
        var executable = Path.Combine(workerDirectory, "regain-alpaca.exe"); Assert.True(File.Exists(executable));
        uint? candidate = null;
        HubClient? first = null, second = null;
        try {
            var attachment = await HubAttachment.AttachAsync(executable, path, workerDirectory);
            candidate = attachment.StartedProcessId; Assert.NotNull(candidate);
            first = await HubClient.ConnectAsync(attachment);
            var next = await HubAttachment.AttachAsync(executable, path, workerDirectory);
            Assert.Null(next.StartedProcessId);
            Assert.Equal(attachment.HostInstance, next.HostInstance);
            second = await HubClient.ConnectAsync(next);
            Assert.NotEqual(first.Hello.ClientId, second.Hello.ClientId);
            var output = Guid.Parse(config["outputs"]![0]!["id"]!.GetValue<string>());
            var connect = JsonSerializer.SerializeToElement(new { op = "changeConnection", output, connected = true, asynchronous = false });
            await first.RequestAsync(connect); await second.RequestAsync(connect);
            var connected = JsonSerializer.SerializeToElement(new { op = "get", output, property = new { member = "connected" } });
            Assert.True((await first.RequestAsync(connected)).GetBoolean());
            var source = Guid.Parse(config["sources"]![0]!["id"]!.GetValue<string>());
            var sourceStatus = JsonSerializer.SerializeToElement(new { op = "sourceStatus", source });
            Assert.Equal(2, (await second.RequestAsync(sourceStatus)).GetProperty("leaseCount").GetInt32());
            first.Dispose();
            Assert.True((await second.RequestAsync(connected)).GetBoolean());
            await Task.Run(async () => {
                using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(3));
                while ((await second.RequestAsync(sourceStatus, deadline.Token)).GetProperty("leaseCount").GetInt32() != 1)
                    await Task.Delay(10, deadline.Token);
            });
            var status = await second.RequestAsync(Command("hostStatus"));
            Assert.Equal("ready", status.GetProperty("phase").GetString());
            Assert.Equal(attachment.ConfigurationRevision, HubWire.Identity(status, "configurationRevision"));
            await second.RequestAsync(JsonSerializer.SerializeToElement(new { op = "changeConnection", output, connected = false, asynchronous = false }));
            Assert.False((await second.RequestAsync(connected)).GetBoolean());
            second.Dispose();
            using var probe = await HubClient.ConnectAsync(next);
            Assert.Equal(attachment.HostInstance, probe.Hello.HostInstance);
        } finally {
            first?.Dispose(); second?.Dispose();
            // Test-only cleanup: this unique configuration contains simulation
            // only, and this sequential test launched its sole candidate. No
            // production frontend uses candidate PID as ownership authority.
            if (candidate is uint pid) {
                try {
                    using var host = Process.GetProcessById(checked((int)pid));
                    Assert.Equal(Path.GetFullPath(executable), host.MainModule!.FileName, ignoreCase: true);
                    host.Kill(); await host.WaitForExitAsync();
                } catch (ArgumentException) { }
            }
            Directory.Delete(directory, true);
        }
    }
    private static string Repository()
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);
        while (directory is not null && !File.Exists(Path.Combine(directory.FullName, "Cargo.toml"))) directory = directory.Parent;
        return directory?.FullName ?? throw new InvalidOperationException("Repository unavailable");
    }
}
