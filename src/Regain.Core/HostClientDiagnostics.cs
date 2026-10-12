using System.Diagnostics;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace Regain.Core;

/// <summary>Metadata-only IPC timings and receipt context; never command arguments or pixels.</summary>
internal sealed class HostClientDiagnostics(Action<string>? sink, string backend, int hostPid, bool supervised)
{
    private const string Prefix = "REGAIN_DIAGNOSTIC ";
    private sealed record Context(string Backend, string? Camera = null, long Capture = 0, long RequestId = 0);
    private readonly object sync = new();
    internal static bool VerboseEnabled => !new[] { "0", "false", "off" }.Contains(
        Environment.GetEnvironmentVariable("REGAIN_CAMERA_TRANSFER_LOG")?.Trim(), StringComparer.OrdinalIgnoreCase);
    private Context context = new(backend);

    internal void Begin(long id, string method, byte[] request)
    {
        if (sink is null) return;
        try
        {
            using var document = JsonDocument.Parse(request);
            Begin(id, method, document.RootElement.GetProperty("params"), request.Length);
        }
        catch { /* Logging must not change IPC or capture results. */ }
    }

    internal void Begin(long id, string method, JsonElement parameters, int requestBytes)
    {
        if (sink is null) return;
        try
        {
            lock (sync)
            {
                string? camera = context.Camera;
                if (method == "open" && parameters.ValueKind == JsonValueKind.Object
                    && parameters.TryGetProperty("name", out var name) && name.ValueKind == JsonValueKind.String)
                    camera = name.GetString();
                context = context with { Camera = camera, RequestId = id,
                    Capture = method == "start" ? context.Capture + 1 : context.Capture };
            }
            Emit("ipc.command.submit", method, id, 0, new { requestJsonBytes = requestBytes });
        }
        catch { /* Logging must not change IPC or capture results. */ }
    }

    internal void Response(long id, string method, JsonElement response, int jsonBytes, int binaryBytes, long started)
    {
        if (sink is null) return;
        try
        {
            if (response.TryGetProperty("result", out var result) && result.ValueKind == JsonValueKind.Object)
            {
                lock (sync)
                {
                    string selectedBackend = context.Backend;
                    if (result.TryGetProperty("backend", out var actual) && actual.ValueKind == JsonValueKind.String)
                        selectedBackend = actual.GetString()!;
                    context = context with { Backend = selectedBackend };
                }
            }
            Emit("ipc.command.response", method, id, ElapsedUs(started), new {
                responseJsonBytes = jsonBytes, binaryBytes,
                ok = response.TryGetProperty("ok", out var ok) && ok.ValueKind == JsonValueKind.True
            });
        }
        catch { /* Logging must not change IPC or capture results. */ }
    }

    internal void Complete(long id, string method, int binaryBytes, long started) =>
        Emit("ipc.command.complete", method, id, ElapsedUs(started), new { binaryBytes });

    internal void Failed(long id, string method, Exception error, long started) =>
        Emit("ipc.command.failed", method, id, ElapsedUs(started), new {
            errorType = error.GetType().Name,
            sdkCode = (error as SdkException)?.Code,
            sdkOperation = (error as SdkException)?.Operation
        });

    internal void Worker(string line)
    {
        if (sink is null) return;
        try
        {
            if (line.StartsWith(Prefix, StringComparison.Ordinal))
            {
                try
                {
                    if (JsonNode.Parse(line[Prefix.Length..]) is JsonObject record)
                    {
                        // Original IDs, timing, backend, and every unknown field are
                        // authoritative. Receipt context is separate and additive.
                        if (!record.ContainsKey("managedContext"))
                            record["managedContext"] = JsonSerializer.SerializeToNode(Snapshot());
                        Write(Prefix + record.ToJsonString());
                        return;
                    }
                }
                catch (JsonException) { /* Preserve malformed/legacy stderr verbatim. */ }
            }
            Write(line);
        }
        catch { /* Stderr diagnostics must not fail the asynchronous reader. */ }
    }

    private object Snapshot()
    {
        Context observed;
        lock (sync) observed = context;
        return new { backend = observed.Backend, hostPid, supervised, camera = observed.Camera,
            capture = observed.Capture, requestId = observed.RequestId,
            correlation = $"{Environment.ProcessId}:{hostPid}:{observed.RequestId}" };
    }

    private void Emit(string kind, string method, long id, double elapsedUs, object details)
    {
        if (sink is null || (!VerboseEnabled && kind != "ipc.command.failed")) return;
        try
        {
            Write(Prefix + JsonSerializer.Serialize(new {
                version = 1, level = "info", @event = kind, message = $"Managed IPC {method}: {kind}",
                pid = Environment.ProcessId, timestampUnixUs = (DateTime.UtcNow.Ticks - DateTime.UnixEpoch.Ticks) / 10,
                elapsedUs, method, requestId = id, managedContext = Snapshot(), details
            }));
        }
        catch { /* Logging must not change IPC or capture results. */ }
    }

    private static double ElapsedUs(long started) => Stopwatch.GetElapsedTime(started).TotalMicroseconds;
    private void Write(string line)
    {
        try { sink?.Invoke(line); }
        catch { /* An unavailable log sink is not a device fault. */ }
    }
}
