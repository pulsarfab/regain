using System.IO;
using System.IO.Pipes;
using System.Text.Json;
using System.Text.Json.Nodes;
using Regain.Hub;

namespace Regain.TestFixtures;

// The same private peer runs inside .NET 8 tests and real net48 x86/x64.
// It never starts a hub, discovers devices or activates equipment drivers.
internal static class HubImageFixture
{
    private static void Check(bool condition, string message)
    { if (!condition) throw new InvalidOperationException(message); }
    private static async Task Failure(HubFailure failure, Task<HubCameraImage> pending)
    {
        try { using var image = await pending; throw new InvalidOperationException("Invalid image was published"); }
        catch (HubException error) { Check(error.Failure == failure, $"Expected {failure}, got {error.Failure}"); }
    }
    internal static async Task RunAll()
    {
        await Types(); await Lifetime(); await Capacity(); await ArrayLifetime();
        foreach (var fault in Faults) await Malformed(fault);
        await Stalled(true); await Stalled(false);
        Console.WriteLine("Image peer: nine types, packed Int32, rank 3, pins, shared budget, malformed input and cancellation/deadline passed");
    }
    internal static readonly string[] Faults = ["host", "revision", "request", "payload", "chunk", "timeout", "order",
        "conversion", "oversize", "unknown", "duplicate", "version", "error", "client", "server", "offset", "type", "transmission", "rank", "width", "height", "planes", "truncated", "trailing"];
    internal static async Task Types()
    {
        foreach (var type in Enumerable.Range(1, 9).Select(value => (HubImageElementType)value)) {
            foreach (var planes in new int?[] { null, 1, 3 }) {
                using var peer = await Peer.Open(type, type, planes);
                var budget = new HubImageBudget(peer.Pixels.Length);
                var serving = peer.Serve();
                using (var image = await peer.Download(budget)) {
                    await serving;
                    Check(image.Descriptor.ElementType == type && image.Descriptor.TransmissionType == type, "Numeric type changed");
                    Check(image.Descriptor.Rank == (planes.HasValue ? 3 : 2) && image.Descriptor.Planes == planes, "Rank changed");
                    Check(image.Request.Acquisition == peer.Request.Acquisition, "Acquisition changed");
                    var copy = new byte[image.ByteLength]; image.CopyTo(0, copy, 0, copy.Length);
                    Check(copy.SequenceEqual(peer.Pixels), "Numeric bytes changed");
                    copy[0] ^= 255; image.CopyTo(0, copy, 0, 1);
                    Check(copy[0] == peer.Pixels[0], "Reader exposed mutable backing storage");
                    Check(budget.UsedBytes == peer.Pixels.Length, "Live buffer uncharged");
                    var arrays = new HubImageBudget(1024 * 1024);
                    var typed = HubCameraArrays.Convert(image, budget: arrays);
                    var variant = HubCameraArrays.Convert(image, variants: true, budget: arrays);
                    Check(typed.Rank == image.Descriptor.Rank && variant.Rank == typed.Rank &&
                        typed.GetLength(0) == image.Descriptor.Width && typed.GetLength(1) == image.Descriptor.Height,
                        "Returned CLR image shape changed");
                    var encoded = new byte[image.ByteLength]; Buffer.BlockCopy(typed, 0, encoded, 0, encoded.Length);
                    Check(encoded.SequenceEqual(peer.Pixels), "Typed CLR image changed numeric bits or X/Y/plane order");
                    Check(variant.GetType().GetElementType() == typeof(object), "Variant array must contain boxed primitives");
                    foreach (var indices in Coordinates(image.Descriptor)) {
                        var scalar = typed.GetValue(indices)!; var boxed = variant.GetValue(indices)!;
                        Check(boxed.GetType() == scalar.GetType() && boxed.Equals(scalar), "Variant pixel numeric type/value changed");
                    }
                    Check(arrays.UsedBytes > 0 && budget.UsedBytes == peer.Pixels.Length, "Array/encoded accounting changed");
                }
                Check(budget.UsedBytes == 0, "Image budget leaked");
            }
        }
        foreach (var transmitted in new[] { HubImageElementType.Byte, HubImageElementType.Int16, HubImageElementType.UInt16 }) {
            using var peer = await Peer.Open(HubImageElementType.Int32, transmitted);
            var serving = peer.Serve(); using var image = await peer.Download(new HubImageBudget(1024)); await serving;
            Check(image.Descriptor.ElementType == HubImageElementType.Int32 && image.Descriptor.TransmissionType == transmitted, "Packed Int32 type changed");
            var typed = HubCameraArrays.Convert(image, budget: new HubImageBudget(4096));
            var variant = HubCameraArrays.Convert(image, true, new HubImageBudget(4096));
            Check(typed.GetType().GetElementType() == typeof(int), "Packed Int32 was not widened to the logical array type");
            var index = 0;
            foreach (var indices in Coordinates(image.Descriptor)) {
                var expected = transmitted switch {
                    HubImageElementType.Byte => (int)peer.Pixels[index++],
                    HubImageElementType.Int16 => BitConverter.ToInt16(peer.Pixels, 2 * index++),
                    _ => BitConverter.ToUInt16(peer.Pixels, 2 * index++)
                };
                Check(typed.GetValue(indices) is int value && value == expected && variant.GetValue(indices) is int boxed && boxed == expected,
                    "Packed Int32 changed signs or variant type");
            }
        }
    }
    private static IEnumerable<int[]> Coordinates(HubImageDescriptor descriptor) {
        for (var x = 0; x < descriptor.Width; x++) for (var y = 0; y < descriptor.Height; y++)
            if (descriptor.Planes.HasValue) for (var p = 0; p < descriptor.Planes.Value; p++) yield return new[] { x, y, p };
            else yield return new[] { x, y };
    }
    internal static async Task ArrayLifetime() {
        using var peer = await Peer.Open(HubImageElementType.Int32, HubImageElementType.UInt16);
        var serving = peer.Serve(); using var image = await peer.Download(new HubImageBudget(4096)); await serving;
        var tiny = new HubImageBudget(1);
        try { HubCameraArrays.Convert(image, budget: tiny); throw new InvalidOperationException("Uncharged CLR array was allocated"); }
        catch (HubException error) { Check(error.Failure == HubFailure.Busy && tiny.UsedBytes == 0, "Array capacity cleanup changed"); }
        var budget = new HubImageBudget(4096);
        using (var stop = new CancellationTokenSource()) {
            stop.Cancel();
            try { HubCameraArrays.Convert(image, budget: budget, cancellation: stop.Token); throw new InvalidOperationException("Cancelled conversion was returned"); }
            catch (OperationCanceledException) { Check(budget.UsedBytes == 0, "Cancelled array retained budget"); }
        }
        var abandoned = AbandonArray(image, budget);
        for (var attempt = 0; attempt < 10 && budget.UsedBytes != 0; attempt++) {
            GC.Collect(); GC.WaitForPendingFinalizers(); GC.Collect(); await Task.Delay(5);
        }
        Check(!abandoned.IsAlive && budget.UsedBytes == 0, "Collected CLR array leaked its budget");
    }
    [System.Runtime.CompilerServices.MethodImpl(System.Runtime.CompilerServices.MethodImplOptions.NoInlining)]
    private static WeakReference AbandonArray(HubCameraImage image, HubImageBudget budget) {
        var array = HubCameraArrays.Convert(image, budget: budget);
        var weak = new WeakReference(array);
        Check(budget.UsedBytes > 0, "Returned array lost its budget reservation");
        // Assert while the array is strongly rooted; after return a background
        // collection is permitted, including before the caller's first read.
        GC.KeepAlive(array);
        return weak;
    }
    internal static async Task Lifetime()
    {
        using var peer = await Peer.Open(width: 70000);
        var budget = new HubImageBudget(peer.Pixels.Length);
        var serving = peer.Serve(); var image = await peer.Download(budget); await serving;
        using var pin = image.Pin(); image.Dispose(); image.Dispose();
        Check(budget.UsedBytes == peer.Pixels.Length, "Original dispose released another reader");
        try { image.Pin(); throw new InvalidOperationException("Disposed reader pinned"); } catch (ObjectDisposedException) { }
        var copy = new byte[HubCameraImages.ChunkBytes];
        pin.CopyTo(17, copy, 0, copy.Length);
        Check(copy.SequenceEqual(peer.Pixels.Skip(17).Take(copy.Length)), "Chunk copy changed order");
        try { pin.CopyTo(0, new byte[65537], 0, 65537); throw new InvalidOperationException("Unbounded copy allowed"); } catch (ArgumentOutOfRangeException) { }
        try { pin.CopyTo(pin.ByteLength, copy, 0, 1); throw new InvalidOperationException("Out of range copy allowed"); } catch (ArgumentOutOfRangeException) { }
        await Task.WhenAll(Enumerable.Range(0, 16).Select(_ => Task.Run(() => {
            using var reader = pin.Pin(); var bytes = new byte[257]; reader.CopyTo(41, bytes, 0, bytes.Length);
            Check(bytes.SequenceEqual(peer.Pixels.Skip(41).Take(bytes.Length)), "Concurrent reader changed pixels");
        })));
        Check(budget.UsedBytes == peer.Pixels.Length, "Pins double charged the buffer");
        var abandoned = Abandon(pin.Pin());
        for (var attempt = 0; attempt < 10 && abandoned.IsAlive; attempt++) {
            GC.Collect(); GC.WaitForPendingFinalizers(); await Task.Delay(10);
        }
        Check(!abandoned.IsAlive && budget.UsedBytes == peer.Pixels.Length, "Abandoned pin was rooted or released another reader");
        pin.Dispose(); Check(budget.UsedBytes == 0, "Last reader did not release budget");
    }
    [System.Runtime.CompilerServices.MethodImpl(System.Runtime.CompilerServices.MethodImplOptions.NoInlining)]
    private static WeakReference Abandon(HubCameraImage image)
    {
        var forgotten = image.Pin(); image.Dispose(); return new WeakReference(forgotten);
    }
    internal static async Task Capacity()
    {
        using var first = await Peer.Open(); using var second = await Peer.Open();
        var budget = new HubImageBudget(first.Pixels.Length);
        var serving = first.Serve(); var image = await first.Download(budget); await serving;
        var pin = image.Pin(); image.Dispose();
        serving = second.Serve(headersOnly: true);
        await Failure(HubFailure.Busy, second.Download(budget)); await serving;
        Check(second.Observed.PixelBuffer is null && budget.UsedBytes == first.Pixels.Length, "Rejected buffer allocated or changed retained charge");
        pin.Dispose(); Check(budget.UsedBytes == 0, "Capacity did not return");
        using var third = await Peer.Open(); serving = third.Serve();
        using (var restored = await third.Download(budget)) { await serving; Check(budget.UsedBytes == third.Pixels.Length, "Returned capacity unusable"); }
        Check(budget.UsedBytes == 0, "Reused capacity leaked");
    }
    internal static async Task Malformed(string fault)
    {
        using var peer = await Peer.Open();
        // Header/manifest errors must be rejected before reserve. A one-byte
        // budget would otherwise turn these into Busy instead of Protocol.
        var bodyFault = fault is "truncated" or "trailing";
        var budget = new HubImageBudget(bodyFault ? 1024 : 1);
        var serving = peer.Serve(fault, allowReject: true);
        await Failure(fault == "truncated" ? HubFailure.Disconnected : HubFailure.Protocol, peer.Download(budget));
        await serving;
        Check(budget.UsedBytes == 0, "Malformed image leaked budget");
        if (!bodyFault) Check(peer.Observed.PixelBuffer is null, "Invalid binary contract allocated pixels");
        if (peer.Observed.PixelBuffer is { } pixels) Check(pixels.All(value => value == 0), "Rejected pixels were not cleared");
    }
    internal static async Task Stalled(bool cancel)
    {
        using var peer = await Peer.Open(width: 70000);
        var budget = new HubImageBudget(peer.Pixels.Length);
        using var stop = new CancellationTokenSource();
        var serving = peer.Serve(headersOnly: true, partial: true);
        var pending = peer.Download(budget, stop.Token, TimeSpan.FromSeconds(3));
        await serving;
        await Bounded(peer.Observed.PixelRead.Task);
        Check(budget.UsedBytes == peer.Pixels.Length, "Partial download uncharged");
        Check(peer.Observed.PixelBuffer!.Any(value => value != 0), "Stall did not retain actual partial pixels");
        if (cancel) {
            stop.Cancel();
            try { using var image = await pending; throw new InvalidOperationException("Cancelled image published"); }
            catch (OperationCanceledException) { }
        } else await Failure(HubFailure.Timeout, pending);
        Check(peer.Observed.Closed && budget.UsedBytes == 0, "Stalled download retained reader or charge");
        Check(peer.Observed.PixelBuffer!.All(value => value == 0), "Partial pixels were not cleared");
    }
    private static async Task Bounded(Task task)
    { if (await Task.WhenAny(task, Task.Delay(10000)) != task) throw new TimeoutException("Image fixture did not reach its ordered boundary"); await task; }
    private static string Name(HubImageElementType type) => type switch {
        HubImageElementType.Int16 => "int16", HubImageElementType.Int32 => "int32", HubImageElementType.Double => "double",
        HubImageElementType.Single => "single", HubImageElementType.UInt64 => "uInt64", HubImageElementType.Byte => "byte",
        HubImageElementType.Int64 => "int64", HubImageElementType.UInt16 => "uInt16", HubImageElementType.UInt32 => "uInt32", _ => throw new Exception()
    };
    private sealed class Peer : IDisposable
    {
        private readonly NamedPipeServerStream server;
        private readonly Guid instance = Guid.NewGuid();
        private readonly JsonObject descriptor;
        internal readonly HubImageRequest Request = new(Guid.NewGuid(), Guid.NewGuid(), Guid.NewGuid(), Guid.NewGuid(), Guid.NewGuid(), Guid.NewGuid(), Guid.NewGuid());
        internal readonly byte[] Pixels;
        internal readonly ObservedStream Observed;
        private Peer(string name, HubImageElementType element, HubImageElementType transmission, int? planes, int width)
        {
            server = new NamedPipeServerStream(name, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous, 65536, 65536);
            Observed = new ObservedStream(new NamedPipeClientStream(".", name, PipeDirection.InOut, PipeOptions.Asynchronous));
            descriptor = new JsonObject { ["width"] = width, ["height"] = 2, ["planes"] = planes,
                ["elementType"] = Name(element), ["transmissionType"] = Name(transmission), ["order"] = "ascom" };
            Pixels = Enumerable.Range(0, width * 2 * (planes ?? 1) * HubImageDescriptor.Size(transmission)).Select(index => (byte)(index * 37 + 129)).ToArray();
        }
        internal static async Task<Peer> Open(HubImageElementType element = HubImageElementType.Byte,
            HubImageElementType transmission = HubImageElementType.Byte, int? planes = null, int width = 3)
        {
            var peer = new Peer("Regain.Image.Fixture." + Guid.NewGuid().ToString("N"), element, transmission, planes, width);
            try {
                var accepting = peer.server.WaitForConnectionAsync();
                await ((NamedPipeClientStream)peer.Observed.Inner).ConnectAsync(10000); await Bounded(accepting); return peer;
            } catch { peer.Dispose(); throw; }
        }
        internal Task<HubCameraImage> Download(HubImageBudget budget, CancellationToken stop = default, TimeSpan? deadline = null)
            => HubCameraImages.FromStreamAsync(Observed, instance, Request, budget, deadline ?? TimeSpan.FromSeconds(15), stop);
        private async Task<JsonElement> Read()
        {
            using var stop = new CancellationTokenSource(TimeSpan.FromSeconds(10));
            return HubWire.Parse(await HubWire.ReadFrame(server, HubWire.MaxFrame, TimeSpan.FromSeconds(5), stop.Token));
        }
        private Task Reply(int id, JsonObject value) => HubWire.WriteFrame(server,
            JsonSerializer.SerializeToUtf8Bytes(new { version = 1, id, result = value }), TimeSpan.FromSeconds(5), CancellationToken.None);
        internal async Task Serve(string? fault = null, bool headersOnly = false, bool allowReject = false, bool partial = false)
        {
            try {
                var helloRequest = await Read();
                Check(helloRequest.GetProperty("id").GetInt32() == 1 && helloRequest.GetProperty("command").GetProperty("op").GetString() == "hello", "Image did not handshake first");
                var hello = new JsonObject { ["protocolVersion"] = 1, ["instanceId"] = instance.ToString(),
                    ["hostInstance"] = (fault == "host" ? Guid.NewGuid() : Request.HostInstance).ToString(),
                    ["configurationRevision"] = (fault == "revision" ? Guid.NewGuid() : Request.ConfigurationRevision).ToString(),
                    ["clientId"] = Guid.NewGuid().ToString(), ["maxFrameBytes"] = 1048576, ["maxInFlight"] = 2,
                    ["operations"] = new JsonArray("cameraImage"), ["capabilities"] = new JsonArray("cameraImageStream") };
                await Reply(1, hello);
                if (fault is "host" or "revision") return;
                var imageRequest = await Read();
                Check(imageRequest.GetProperty("id").GetInt32() == 2 && imageRequest.GetProperty("command").GetProperty("op").GetString() == "cameraImage", "Image request sequence changed");
                Request.Match(imageRequest.GetProperty("command").GetProperty("request"));
                var echoed = JsonSerializer.SerializeToNode(Request.Wire())!.AsObject();
                if (fault == "request") echoed["acquisition"] = Guid.NewGuid().ToString();
                var wireDescriptor = descriptor.DeepClone().AsObject();
                if (fault == "order") wireDescriptor["order"] = "sensorRows";
                if (fault == "conversion") wireDescriptor["elementType"] = "double";
                if (fault == "oversize") { wireDescriptor["width"] = int.MaxValue; wireDescriptor["height"] = int.MaxValue; wireDescriptor["planes"] = int.MaxValue; }
                var manifest = new JsonObject { ["request"] = echoed, ["descriptor"] = wireDescriptor,
                    ["payloadBytes"] = Pixels.Length + 44 + (fault == "payload" ? 1 : 0),
                    ["chunkBytes"] = fault == "chunk" ? 1 : 65536, ["timeoutSeconds"] = fault == "timeout" ? 301 : 300 };
                if (fault == "unknown") manifest["extra"] = 1;
                if (fault == "duplicate") {
                    var json = JsonSerializer.Serialize(new { version = 1, id = 2, result = manifest });
                    json = json.Replace("\"payloadBytes\":", "\"payloadBytes\":0,\"payloadBytes\":");
                    await HubWire.WriteFrame(server, System.Text.Encoding.UTF8.GetBytes(json), TimeSpan.FromSeconds(5), CancellationToken.None);
                } else await Reply(2, manifest);
                var element = new HubImageDescriptor(JsonSerializer.SerializeToElement(descriptor));
                uint[] header = [1, 0, 2, 2, 44, (uint)element.ElementType, (uint)element.TransmissionType,
                    (uint)element.Rank, (uint)element.Width, (uint)element.Height, (uint)(element.Planes ?? 0)];
                var index = fault switch { "version" => 0, "error" => 1, "client" => 2, "server" => 3, "offset" => 4,
                    "type" => 5, "transmission" => 6, "rank" => 7, "width" => 8, "height" => 9, "planes" => 10, _ => -1 };
                if (index >= 0) header[index]++;
                var bytes = header.SelectMany(value => new[] { (byte)value, (byte)(value >> 8), (byte)(value >> 16), (byte)(value >> 24) }).ToArray();
                await server.WriteAsync(bytes, 0, bytes.Length);
                if (headersOnly) {
                    if (partial) await server.WriteAsync(Pixels, 0, 257);
                    await server.FlushAsync(); return;
                }
                await server.WriteAsync(Pixels, 0, fault == "truncated" ? Pixels.Length - 1 : Pixels.Length);
                if (fault == "trailing") await server.WriteAsync(new byte[] { 42 }, 0, 1);
                await server.FlushAsync(); server.Dispose();
            } catch (Exception error) when (allowReject && (error is IOException || error is ObjectDisposedException || error is HubException hub && hub.Failure == HubFailure.Disconnected)) { }
        }
        public void Dispose() { Observed.Dispose(); server.Dispose(); }
    }
    private sealed class ObservedStream(Stream inner) : Stream
    {
        internal readonly Stream Inner = inner;
        internal readonly TaskCompletionSource<bool> PixelRead = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal byte[]? PixelBuffer;
        internal bool Closed;
        public override async Task<int> ReadAsync(byte[] buffer, int offset, int count, CancellationToken token)
        {
            // Pixel buffers are first requested after the complete 44-byte header.
            // Descriptor cases use six bytes, larger cases one 64 KiB chunk.
            var pixelRead = count == 6 || count == 65536;
            if (pixelRead) PixelBuffer = buffer;
            var received = await Inner.ReadAsync(buffer, offset, count, token).ConfigureAwait(false);
            if (pixelRead && received > 0) PixelRead.TrySetResult(true);
            return received;
        }
        protected override void Dispose(bool disposing) { Closed = true; if (disposing) Inner.Dispose(); base.Dispose(disposing); }
        public override bool CanRead => Inner.CanRead;
        public override bool CanWrite => Inner.CanWrite;
        public override bool CanSeek => false;
        public override long Length => throw new NotSupportedException();
        public override long Position { get => throw new NotSupportedException(); set => throw new NotSupportedException(); }
        public override int Read(byte[] buffer, int offset, int count) => Inner.Read(buffer, offset, count);
        public override void Write(byte[] buffer, int offset, int count) => Inner.Write(buffer, offset, count);
        public override Task WriteAsync(byte[] buffer, int offset, int count, CancellationToken token) => Inner.WriteAsync(buffer, offset, count, token);
        public override void Flush() => Inner.Flush();
        public override Task FlushAsync(CancellationToken token) => Inner.FlushAsync(token);
        public override long Seek(long offset, SeekOrigin origin) => throw new NotSupportedException();
        public override void SetLength(long value) => throw new NotSupportedException();
    }
}
