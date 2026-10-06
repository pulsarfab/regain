using System.IO;
using System.Text.Json;

namespace Regain.Hub;

public enum HubFailure { Disconnected, Timeout, Protocol, Busy, InvalidRequest, Uncertain, Remote }

public sealed class HubException : Exception
{
    public HubFailure Failure { get; }
    public HubRemoteError? Remote { get; }
    internal HubException(HubFailure failure, HubRemoteError? remote = null) : base(failure switch {
        HubFailure.Timeout => "Hub request deadline expired",
        HubFailure.Protocol => "Hub protocol or identity is invalid",
        HubFailure.Busy => "Hub client request limit reached",
        HubFailure.InvalidRequest => "Hub request is invalid, too large, or not advertised",
        HubFailure.Uncertain => "Hub operation may have completed; reconcile before another command",
        HubFailure.Remote => "Hub rejected the request; inspect its structured error",
        _ => "Hub connection is closed; attach again"
    }) { Failure = failure; Remote = remote; }
}

public sealed class HubRemoteError
{
    public string Code { get; }
    public string Message { get; }
    public int? UpstreamCode { get; }
    public double? RetryAfterSeconds { get; }
    public JsonElement Fields { get; }
    internal HubRemoteError(JsonElement value)
    {
        HubWire.Members(value, "code", "message", "upstreamCode", "retryAfterSeconds", "fields");
        Code = value.GetProperty("code").GetString() ?? throw new HubException(HubFailure.Protocol);
        Message = value.GetProperty("message").GetString() ?? throw new HubException(HubFailure.Protocol);
        if (Code.Length == 0) throw new HubException(HubFailure.Protocol);
        if (value.TryGetProperty("upstreamCode", out var upstream) && upstream.ValueKind != JsonValueKind.Null)
            UpstreamCode = upstream.GetInt32();
        if (value.TryGetProperty("retryAfterSeconds", out var delay) && delay.ValueKind != JsonValueKind.Null) {
            var seconds = delay.GetDouble();
            if (double.IsNaN(seconds) || double.IsInfinity(seconds) || seconds < 0) throw new HubException(HubFailure.Protocol);
            RetryAfterSeconds = seconds;
        }
        if (value.TryGetProperty("fields", out var fields)) {
            if (fields.ValueKind != JsonValueKind.Array) throw new HubException(HubFailure.Protocol);
            Fields = fields.Clone();
        } else Fields = JsonSerializer.SerializeToElement(Array.Empty<object>());
    }
}

public sealed class HubHello
{
    public Guid InstanceId { get; }
    public Guid HostInstance { get; }
    public Guid ConfigurationRevision { get; }
    public Guid ClientId { get; }
    public int MaxFrameBytes { get; }
    public int MaxInFlight { get; }
    public IReadOnlyList<string> Operations { get; }
    public IReadOnlyList<string> Capabilities { get; }
    internal HubHello(JsonElement value, Guid expected)
    {
        HubWire.Members(value, "protocolVersion", "instanceId", "hostInstance", "configurationRevision",
            "clientId", "maxFrameBytes", "maxInFlight", "operations", "capabilities");
        InstanceId = HubWire.Identity(value, "instanceId");
        HostInstance = HubWire.Identity(value, "hostInstance");
        ConfigurationRevision = HubWire.Identity(value, "configurationRevision");
        ClientId = HubWire.Identity(value, "clientId");
        MaxFrameBytes = value.GetProperty("maxFrameBytes").GetInt32();
        MaxInFlight = value.GetProperty("maxInFlight").GetInt32();
        if (value.GetProperty("protocolVersion").GetInt32() != 1 || InstanceId != expected ||
            MaxFrameBytes <= 0 || MaxFrameBytes > HubWire.MaxFrame || MaxInFlight <= 0 || MaxInFlight > 8)
            throw new HubException(HubFailure.Protocol);
        Operations = Array.AsReadOnly(Strings(value, "operations"));
        Capabilities = Array.AsReadOnly(Strings(value, "capabilities"));
    }
    private static string[] Strings(JsonElement value, string name)
    {
        var array = value.GetProperty(name);
        if (array.ValueKind != JsonValueKind.Array || array.GetArrayLength() > 128) throw new HubException(HubFailure.Protocol);
        var strings = array.EnumerateArray().Select(v => v.GetString() ?? "").ToArray();
        if (strings.Any(v => v.Length == 0 || v.Length > 256) || strings.Distinct(StringComparer.Ordinal).Count() != strings.Length)
            throw new HubException(HubFailure.Protocol);
        return strings;
    }
}

public sealed class HubClientLimits
{
    public TimeSpan FrameTimeout { get; }
    public TimeSpan RequestTimeout { get; }
    public HubClientLimits(TimeSpan? frameTimeout = null, TimeSpan? requestTimeout = null)
    {
        FrameTimeout = frameTimeout ?? TimeSpan.FromSeconds(5);
        RequestTimeout = requestTimeout ?? TimeSpan.FromSeconds(35);
        if (FrameTimeout <= TimeSpan.Zero || FrameTimeout > TimeSpan.FromSeconds(60) ||
            RequestTimeout <= TimeSpan.Zero || RequestTimeout > TimeSpan.FromSeconds(300))
            throw new HubException(HubFailure.InvalidRequest);
    }
}

internal static class HubWire
{
    internal const int MaxFrame = 1024 * 1024;
    internal static Guid Identity(JsonElement value, string key)
    {
        if (!Guid.TryParse(value.GetProperty(key).GetString(), out var id) || id == Guid.Empty)
            throw new HubException(HubFailure.Protocol);
        return id;
    }
    internal static void Members(JsonElement value, params string[] allowed)
    {
        if (value.ValueKind != JsonValueKind.Object || value.EnumerateObject().Any(p => !allowed.Contains(p.Name, StringComparer.Ordinal)))
            throw new HubException(HubFailure.Protocol);
    }
    internal static JsonElement Parse(byte[] bytes)
    {
        try {
            using var document = JsonDocument.Parse(bytes, new JsonDocumentOptions { MaxDepth = 64 });
            Unique(document.RootElement);
            return document.RootElement.Clone();
        } catch (JsonException) { throw new HubException(HubFailure.Protocol); }
    }
    internal static void Unique(JsonElement value)
    {
        if (value.ValueKind == JsonValueKind.Object) {
            var names = new HashSet<string>(StringComparer.Ordinal);
            foreach (var property in value.EnumerateObject()) {
                if (!names.Add(property.Name)) throw new HubException(HubFailure.Protocol);
                Unique(property.Value);
            }
        } else if (value.ValueKind == JsonValueKind.Array)
            foreach (var item in value.EnumerateArray()) Unique(item);
    }
    internal static async Task<byte[]> ReadFrame(Stream stream, int maximum, TimeSpan partialTimeout, CancellationToken stop)
    {
        var header = new byte[4];
        // An idle established stream is valid. Start the partial-frame deadline
        // only after the first byte arrives, then close even a stalled read.
        if (await stream.ReadAsync(header, 0, 1, stop).ConfigureAwait(false) != 1) throw new HubException(HubFailure.Disconnected);
        using var timer = CancellationTokenSource.CreateLinkedTokenSource(stop);
        timer.CancelAfter(partialTimeout);
        using var closing = timer.Token.Register(() => { try { stream.Dispose(); } catch { } });
        try {
            await Exact(stream, header, 1, 3, timer.Token).ConfigureAwait(false);
            var length = (uint)(header[0] | header[1] << 8 | header[2] << 16 | header[3] << 24);
            if (length == 0 || length > maximum) throw new HubException(HubFailure.Protocol);
            var bytes = new byte[(int)length];
            try { await Exact(stream, bytes, 0, bytes.Length, timer.Token).ConfigureAwait(false); return bytes; }
            catch { Array.Clear(bytes, 0, bytes.Length); throw; }
        } catch (Exception) when (timer.IsCancellationRequested) {
            throw new HubException(stop.IsCancellationRequested ? HubFailure.Disconnected : HubFailure.Timeout);
        }
    }
    private static async Task Exact(Stream stream, byte[] bytes, int offset, int count, CancellationToken token)
    {
        while (count != 0) {
            var read = await stream.ReadAsync(bytes, offset, count, token).ConfigureAwait(false);
            if (read == 0) throw new HubException(HubFailure.Disconnected);
            offset += read; count -= read;
        }
    }
    internal static async Task WriteFrame(Stream stream, byte[] bytes, TimeSpan timeout, CancellationToken stop)
    {
        using var timer = CancellationTokenSource.CreateLinkedTokenSource(stop);
        timer.CancelAfter(timeout);
        using var closing = timer.Token.Register(() => { try { stream.Dispose(); } catch { } });
        try {
            var size = bytes.Length;
            byte[] header = [(byte)size, (byte)(size >> 8), (byte)(size >> 16), (byte)(size >> 24)];
            await stream.WriteAsync(header, 0, 4, timer.Token).ConfigureAwait(false);
            await stream.WriteAsync(bytes, 0, bytes.Length, timer.Token).ConfigureAwait(false);
            await stream.FlushAsync(timer.Token).ConfigureAwait(false);
        } catch (Exception) when (timer.IsCancellationRequested) {
            throw new HubException(stop.IsCancellationRequested ? HubFailure.Disconnected : HubFailure.Timeout);
        }
    }

    // Utf8JsonWriter may request a whole escaped string at once. Reject large
    // tokens before writing, then enforce the actual serialized byte ceiling.
    internal static void CheckTokens(JsonElement value, ref long remaining, int depth = 0)
    {
        if (depth > 60 || remaining < 0) throw new HubException(HubFailure.InvalidRequest);
        switch (value.ValueKind) {
            case JsonValueKind.Object:
                remaining -= 2;
                foreach (var p in value.EnumerateObject()) {
                    remaining -= 4 + EscapedLength(p.Name);
                    CheckTokens(p.Value, ref remaining, depth + 1);
                }
                break;
            case JsonValueKind.Array:
                remaining -= 2;
                foreach (var v in value.EnumerateArray()) { remaining--; CheckTokens(v, ref remaining, depth + 1); }
                break;
            case JsonValueKind.String: remaining -= 2 + EscapedLength(value.GetString()!); break;
            case JsonValueKind.Number: remaining -= value.GetRawText().Length; break;
            case JsonValueKind.True: remaining -= 4; break;
            case JsonValueKind.False: remaining -= 5; break;
            case JsonValueKind.Null: remaining -= 4; break;
            default: throw new HubException(HubFailure.InvalidRequest);
        }
        if (remaining < 0) throw new HubException(HubFailure.InvalidRequest);
    }
    private static long EscapedLength(string value)
    {
        long size = 0;
        // The default encoder may escape non-ASCII and HTML characters. This
        // conservative bound never allocates an oversized outgoing frame.
        foreach (var c in value) size += c >= 0x80 || System.Text.Encodings.Web.JavaScriptEncoder.Default.WillEncode(c) ? 6 : 1;
        return size;
    }
    internal static byte[] Encode(ulong id, JsonElement command, int maximum)
    {
        long budget = maximum - 80;
        CheckTokens(command, ref budget);
        using var stream = new MemoryStream();
        try {
            using (var writer = new Utf8JsonWriter(stream)) {
                writer.WriteStartObject(); writer.WriteNumber("version", 1); writer.WriteNumber("id", id);
                writer.WritePropertyName("command"); command.WriteTo(writer); writer.WriteEndObject();
            }
            if (stream.Length > maximum) throw new HubException(HubFailure.InvalidRequest);
            return stream.ToArray();
        } finally {
            if (stream.TryGetBuffer(out var buffer)) Array.Clear(buffer.Array!, buffer.Offset, buffer.Count);
        }
    }
}
