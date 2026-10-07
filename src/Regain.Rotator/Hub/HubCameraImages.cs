using System.IO;
using System.IO.Pipes;
using System.Security.Principal;
using System.Text.Json;

namespace Regain.Hub;

public static class HubCameraImages
{
    public const int ChunkBytes = 64 * 1024;
    public static TimeSpan MaximumTransferTime { get; } = TimeSpan.FromSeconds(300);
    private static readonly TimeSpan frameTime = TimeSpan.FromSeconds(5);

    /// Download an already completed image over a separate verified local pipe.
    /// No equipment connection, capture, Abort or automatic retry is performed.
    public static async Task<HubCameraImage> DownloadAsync(HubAttachment attachment, HubClient control,
        HubImageIdentity request, HubImageBudget budget, TimeSpan deadline, CancellationToken cancellation = default)
    {
        Validate(deadline);
        control.RequireCapabilities("cameraAcquisition", "cameraImageStream");
        if (request is HubGroupImageRequest) control.RequireCapabilities("cameraGroups");
        if (control.Hello.InstanceId != attachment.InstanceId || control.Hello.HostInstance != attachment.HostInstance ||
            request.HostInstance != control.Hello.HostInstance || request.ConfigurationRevision != control.Hello.ConfigurationRevision ||
            request is HubImageRequest ordinary && ordinary.ClientId != control.Hello.ClientId ||
            !control.Hello.Operations.Contains(request.OperationKey, StringComparer.Ordinal))
            throw new HubException(HubFailure.Protocol);
        using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        timer.CancelAfter(deadline);
        using var pipe = new NamedPipeClientStream(".", HubPipe.Name(attachment.Address), PipeDirection.InOut,
            PipeOptions.Asynchronous, TokenImpersonationLevel.Identification, HandleInheritability.None);
        using var closing = timer.Token.Register(() => Close(pipe));
        HubCameraImage? image = null;
        try {
            await pipe.ConnectAsync((int)deadline.TotalMilliseconds, timer.Token).ConfigureAwait(false);
            HubPipe.Verify(pipe.SafePipeHandle);
            image = await FromStreamAsync(pipe, attachment.InstanceId, request, budget, deadline, timer.Token).ConfigureAwait(false);
            cancellation.ThrowIfCancellationRequested();
            if (timer.IsCancellationRequested) throw new HubException(HubFailure.Timeout);
            var result = image; image = null; return result;
        } catch (Exception error) {
            cancellation.ThrowIfCancellationRequested();
            if (timer.IsCancellationRequested || error is TimeoutException) throw new HubException(HubFailure.Timeout);
            throw error is HubException ? error : new HubException(HubFailure.Disconnected);
        } finally { image?.Dispose(); }
    }

    // Owns the injected protected stream, including on validation failure.
    // Shared by both frontend frameworks; tests alone inject a private peer.
    internal static async Task<HubCameraImage> FromStreamAsync(Stream stream, Guid instance, HubImageIdentity request,
        HubImageBudget budget, TimeSpan deadline, CancellationToken cancellation = default)
    {
        using (stream) {
            Validate(deadline);
            if (instance == Guid.Empty) throw new HubException(HubFailure.InvalidRequest);
            using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
            timer.CancelAfter(deadline);
            using var closing = timer.Token.Register(() => Close(stream));
            HubImageBudget.Reservation? reservation = null;
            byte[]? pixels = null;
            try {
                await Write(stream, 1, new { op = "hello" }, HubWire.MaxFrame, timer.Token).ConfigureAwait(false);
                var first = await Json(stream, HubWire.MaxFrame, timer.Token).ConfigureAwait(false);
                HubWire.Members(first, "version", "id", "result"); Envelope(first, 1);
                var hello = new HubHello(first.GetProperty("result"), instance);
                if (hello.HostInstance != request.HostInstance || hello.ConfigurationRevision != request.ConfigurationRevision ||
                    !hello.Capabilities.Contains("cameraImageStream", StringComparer.Ordinal) ||
                    request is HubGroupImageRequest && !hello.Capabilities.Contains("cameraGroups", StringComparer.Ordinal) ||
                    !hello.Operations.Contains(request.OperationKey, StringComparer.Ordinal)) throw new HubException(HubFailure.Protocol);
                await Write(stream, 2, new { op = request.OperationKey, request = request.Wire() }, hello.MaxFrameBytes, timer.Token).ConfigureAwait(false);
                var reply = await Json(stream, hello.MaxFrameBytes, timer.Token).ConfigureAwait(false);
                HubWire.Members(reply, "version", "id", "result", "error"); Envelope(reply, 2);
                var hasValue = reply.TryGetProperty("result", out var manifest);
                var hasError = reply.TryGetProperty("error", out var failure);
                if (hasValue == hasError) throw new HubException(HubFailure.Protocol);
                if (hasError) throw new HubException(HubFailure.Remote, new HubRemoteError(failure));
                HubWire.Members(manifest, "request", "descriptor", "payloadBytes", "chunkBytes", "timeoutSeconds");
                request.Match(manifest.GetProperty("request"));
                var descriptor = new HubImageDescriptor(manifest.GetProperty("descriptor"));
                if (manifest.GetProperty("payloadBytes").GetInt32() != descriptor.ByteLength + 44 ||
                    manifest.GetProperty("chunkBytes").GetInt32() != ChunkBytes || manifest.GetProperty("timeoutSeconds").GetInt32() != 300)
                    throw new HubException(HubFailure.Protocol);
                var header = new byte[44];
                await Exact(stream, header, 0, header.Length, timer.Token).ConfigureAwait(false);
                uint[] fields = Enumerable.Range(0, 11).Select(index => U32(header, index * 4)).ToArray();
                if (fields[0] != 1 || fields[1] != 0 || fields[2] != 2 || fields[3] != 2 || fields[4] != 44 ||
                    fields[5] != (uint)descriptor.ElementType || fields[6] != (uint)descriptor.TransmissionType ||
                    fields[7] != descriptor.Rank || fields[8] != descriptor.Width || fields[9] != descriptor.Height || fields[10] != (descriptor.Planes ?? 0))
                    throw new HubException(HubFailure.Protocol);
                reservation = budget.Reserve(descriptor.ByteLength);
                pixels = new byte[descriptor.ByteLength];
                for (var offset = 0; offset < pixels.Length;) {
                    var count = Math.Min(ChunkBytes, pixels.Length - offset);
                    await Exact(stream, pixels, offset, count, timer.Token).ConfigureAwait(false);
                    offset += count;
                }
                if (await Eof(stream, timer.Token).ConfigureAwait(false) != 0) throw new HubException(HubFailure.Protocol);
                cancellation.ThrowIfCancellationRequested();
                if (timer.IsCancellationRequested) throw new HubException(HubFailure.Timeout);
                var image = new HubCameraImage(request, descriptor, pixels, reservation);
                pixels = null; reservation = null; return image;
            } catch (Exception error) {
                cancellation.ThrowIfCancellationRequested();
                if (timer.IsCancellationRequested) throw new HubException(HubFailure.Timeout);
                if (error is OutOfMemoryException) throw new HubException(HubFailure.Busy);
                if (error is HubException) throw;
                throw new HubException(error is IOException or ObjectDisposedException ? HubFailure.Disconnected : HubFailure.Protocol);
            } finally {
                if (pixels is not null) Array.Clear(pixels, 0, pixels.Length);
                reservation?.Dispose();
            }
        }
    }
    private static void Validate(TimeSpan deadline)
    { if (deadline <= TimeSpan.Zero || deadline > MaximumTransferTime) throw new HubException(HubFailure.InvalidRequest); }
    private static void Envelope(JsonElement value, ulong id)
    { if (value.GetProperty("version").GetInt32() != 1 || value.GetProperty("id").GetUInt64() != id) throw new HubException(HubFailure.Protocol); }
    private static uint U32(byte[] bytes, int offset) => (uint)(bytes[offset] | bytes[offset + 1] << 8 | bytes[offset + 2] << 16 | bytes[offset + 3] << 24);
    private static void Close(Stream stream) { try { stream.Dispose(); } catch { } }
    private static async Task Write(Stream stream, ulong id, object command, int maximum, CancellationToken token)
    {
        var bytes = HubWire.Encode(id, JsonSerializer.SerializeToElement(command), maximum);
        try { await HubWire.WriteFrame(stream, bytes, frameTime, token).ConfigureAwait(false); }
        finally { Array.Clear(bytes, 0, bytes.Length); }
    }
    private static async Task<JsonElement> Json(Stream stream, int maximum, CancellationToken token)
    {
        using var timer = CancellationTokenSource.CreateLinkedTokenSource(token); timer.CancelAfter(frameTime);
        using var closing = timer.Token.Register(() => Close(stream));
        try {
            var bytes = await HubWire.ReadFrame(stream, maximum, frameTime, timer.Token).ConfigureAwait(false);
            try { return HubWire.Parse(bytes); } finally { Array.Clear(bytes, 0, bytes.Length); }
        } catch (Exception) when (timer.IsCancellationRequested && !token.IsCancellationRequested) { throw new HubException(HubFailure.Timeout); }
    }
    private static async Task Exact(Stream stream, byte[] bytes, int offset, int count, CancellationToken token)
    {
        using var timer = CancellationTokenSource.CreateLinkedTokenSource(token); timer.CancelAfter(frameTime);
        using var closing = timer.Token.Register(() => Close(stream));
        try {
            while (count != 0) {
                var received = await stream.ReadAsync(bytes, offset, count, timer.Token).ConfigureAwait(false);
                if (received == 0) throw new HubException(HubFailure.Disconnected);
                offset += received; count -= received;
            }
        } catch (Exception) when (timer.IsCancellationRequested && !token.IsCancellationRequested) { throw new HubException(HubFailure.Timeout); }
    }
    private static async Task<int> Eof(Stream stream, CancellationToken token)
    {
        using var timer = CancellationTokenSource.CreateLinkedTokenSource(token); timer.CancelAfter(frameTime);
        using var closing = timer.Token.Register(() => Close(stream));
        try { return await stream.ReadAsync(new byte[1], 0, 1, timer.Token).ConfigureAwait(false); }
        catch (Exception) when (timer.IsCancellationRequested && !token.IsCancellationRequested) { throw new HubException(HubFailure.Timeout); }
    }
}
