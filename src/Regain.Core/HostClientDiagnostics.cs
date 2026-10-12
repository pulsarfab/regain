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
    private string transferLogMode = "circular";
    internal string TransferLogMode { get => Volatile.Read(ref transferLogMode); set {
        Volatile.Write(ref transferLogMode, value);
        if (value != "circular") lock (sync) { history.Clear(); historyBytes=0; }
    } }
    private readonly Queue<string> history = new();
    private int historyBytes;
    private long lastBatch;

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
                        Route(Prefix + record.ToJsonString(), record["event"]?.GetValue<string>() ?? "", record["level"]?.GetValue<string>() ?? "info", record.ContainsKey("flightRecorderBatch"));
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
        if (sink is null || ((!VerboseEnabled || TransferLogMode == "off") && kind != "ipc.command.failed")) return;
        try
        {
            Route(Prefix + JsonSerializer.Serialize(new {
                version = 1, level = "info", @event = kind, message = $"Managed IPC {method}: {kind}",
                pid = Environment.ProcessId, timestampUnixUs = (DateTime.UtcNow.Ticks - DateTime.UnixEpoch.Ticks) / 10,
                elapsedUs, method, requestId = id, managedContext = Snapshot(), details
            }), kind, kind == "ipc.command.failed" ? "warning" : "info");
        }
        catch { /* Logging must not change IPC or capture results. */ }
    }

    private static double ElapsedUs(long started) => Stopwatch.GetElapsedTime(started).TotalMicroseconds;
    private void Route(string line, string kind, string level, bool replay = false)
    {
        bool highVolume = kind.StartsWith("usb.", StringComparison.Ordinal) || kind.StartsWith("sdk.api.", StringComparison.Ordinal)
            || kind is "ipc.command.submit" or "ipc.command.response" or "ipc.command.complete" or "nina.api.complete" or "nina.image_factory.complete";
        bool fault = level is "warning" or "error" || kind is "ipc.command.failed" or "diagnostics.batch.begin";
        string mode = VerboseEnabled ? TransferLogMode : "off";
        if (mode == "circular" && !replay && highVolume && !fault) {
            string? batch = null;
            lock (sync) {
                if (lastBatch != 0 && Stopwatch.GetElapsedTime(lastBatch).TotalSeconds < 5) batch=$"managed:{Environment.ProcessId}:{hostPid}:{lastBatch}";
                else {
                    history.Enqueue(line); historyBytes += line.Length;
                    while (historyBytes > 2 * 1024 * 1024 && history.TryDequeue(out var old)) historyBytes -= old.Length;
                }
            }
            if (batch is not null) {
                var record=JsonNode.Parse(line[Prefix.Length..])!; record["flightRecorderBatch"]=batch; record["flightRecorderAfterFault"]=true;
                Write(Prefix+record.ToJsonString());
            }
            return;
        }
        if (fault && mode == "circular" && !replay) FlushHistory();
        if (mode != "circular") { lock (sync) {history.Clear(); historyBytes = 0;} }
        if (!replay && mode == "off" && highVolume && !fault) return;
        Write(line);
    }
    private void FlushHistory()
    {
        string[] lines;
        lock (sync) {
            long now = Stopwatch.GetTimestamp();
            if (lastBatch != 0 && Stopwatch.GetElapsedTime(lastBatch).TotalSeconds < 2) return;
            lastBatch = now; lines = history.ToArray(); history.Clear(); historyBytes = 0;
        }
        if (lines.Length == 0) return;
        string batch = $"managed:{Environment.ProcessId}:{hostPid}:{lastBatch}";
        string? path = null;
        try {
            string directory = Environment.GetEnvironmentVariable("REGAIN_CAMERA_DIAGNOSTICS_DIR")
                ?? Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Regain", "Diagnostics");
            Directory.CreateDirectory(directory);
            foreach (var old in Directory.EnumerateFiles(directory, "managed-*-fault.jsonl").OrderByDescending(File.GetLastWriteTimeUtc).Skip(7))
                try { File.Delete(old); } catch { /* Retain active/unavailable files. */ }
            path = Path.Combine(directory, $"managed-{hostPid}-fault.jsonl");
            File.WriteAllLines(path, lines);
        } catch { /* The INFO batch remains available if disk is unavailable. */ }
        Write(Prefix + JsonSerializer.Serialize(new {version=1,level="warning",@event="diagnostics.batch.begin",message="Recent managed camera timing history follows",details=new {batch,records=lines.Length,path}}));
        foreach (var line in lines) {
            try {var record=JsonNode.Parse(line[Prefix.Length..])!; record["flightRecorderBatch"]=batch; Write(Prefix+record.ToJsonString());}
            catch {Write(line);}
        }
        Write(Prefix + JsonSerializer.Serialize(new {version=1,level="info",@event="diagnostics.batch.end",message="End of managed camera timing history",details=new {batch}}));
    }
    private void Write(string line)
    {
        try { sink?.Invoke(line); }
        catch { /* An unavailable log sink is not a device fault. */ }
    }
}
