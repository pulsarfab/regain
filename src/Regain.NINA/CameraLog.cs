using System.Diagnostics;
using System.Text.Json;
using NINA.Core.Utility;

namespace Regain.NINA;

/// <summary>Worker stderr is diagnostic feedback, never an operator notification.</summary>
internal static class CameraLog
{
    internal enum Level { Debug, Info, Warning }
    internal sealed record Entry(Level Severity, string Text, bool IncludeInfo = false, bool Suppressed = false);
    private const string Prefix = "REGAIN_DIAGNOSTIC ";

    internal static bool VerboseEnabled => !new[] { "0", "false", "off" }.Contains(
        Environment.GetEnvironmentVariable("REGAIN_CAMERA_TRANSFER_LOG")?.Trim(), StringComparer.OrdinalIgnoreCase);

    internal static Entry Parse(string backend, string line, bool? verbose = null)
    {
        if (line.StartsWith(Prefix, StringComparison.Ordinal))
        {
            try
            {
                using var document = JsonDocument.Parse(line[Prefix.Length..]);
                var root = document.RootElement;
                if (root.ValueKind == JsonValueKind.Object && root.TryGetProperty("version", out var version)
                    && version.ValueKind == JsonValueKind.Number && version.TryGetInt32(out var number) && number == 1
                    && root.TryGetProperty("message", out var message) && message.ValueKind == JsonValueKind.String
                    && root.TryGetProperty("event", out var kind) && kind.ValueKind == JsonValueKind.String)
                {
                    var severity = root.TryGetProperty("level", out var level) && level.ValueKind == JsonValueKind.String
                        ? level.GetString() switch { "debug" => Level.Debug, "warning" or "error" => Level.Warning, _ => Level.Info }
                        : Level.Info;
                    string process = root.TryGetProperty("pid", out var pid) && pid.ValueKind == JsonValueKind.Number
                        && pid.TryGetInt32(out var id) ? $" process {id}" : "";
                    // Keep the complete structured record, including fields introduced by
                    // newer workers. Transfer/timing evidence must survive the INFO sink
                    // and must not be truncated before JSON parsing.
                    string eventName = kind.GetString()!;
                    bool highVolume = eventName.StartsWith("usb.", StringComparison.Ordinal)
                        || eventName.StartsWith("sdk.api.", StringComparison.Ordinal)
                        || eventName is "ipc.command.submit" or "ipc.command.response" or "ipc.command.complete"
                        or "nina.api.complete" or "nina.image_factory.complete";
                    return new(severity, Clean($"PulsarFab regain {backend}{process} [{eventName}]: {message.GetString()} | {Prefix}{root.GetRawText()}"),
                        IncludeInfo: true, Suppressed: !(verbose ?? VerboseEnabled) && highVolume && severity != Level.Warning);
                }
            }
            catch (JsonException) { /* Keep malformed diagnostics visible as plain text. */ }
            catch (InvalidOperationException) { /* Wrong JSON field types are not camera failures. */ }
        }
        // Only legacy/unstructured output is bounded (including SDK loader failures).
        if (line.Length > 8192) line = line[..8192] + " [truncated]";
        return new(Level.Info, Clean($"PulsarFab regain {backend}: {line}"));
    }

    private static string Clean(string text) => text.Replace('\r', ' ').Replace('\n', ' ');

    internal static void Worker(string backend, string line) => Forward(Parse(backend, line),
        text => Logger.Info(text), text => Logger.Warning(text), text => Logger.Debug(text));

    internal static void Session(string camera, string message) => Forward(new(
        Level.Info, Clean($"PulsarFab regain {camera}: {message}")),
        text => Logger.Info(text), text => Logger.Warning(text), text => Logger.Debug(text));

    internal static Entry ApiTimingEntry(string camera, string backend, long capture, string api, long started,
        string outcome, Exception? error = null, int? width = null, int? height = null, int? pixelCount = null)
    {
        static string Bounded(string text, int length) => text.Length > length ? text[..length] : text;
        string kind = api == "CreateImageArrayExposureData" ? "nina.image_factory.complete" : "nina.api.complete";
        var record = new {
            version = 1, level = outcome == "failed" ? "warning" : "info", @event = kind, message = $"NINA {Bounded(api, 64)} {Bounded(outcome, 32)}",
            pid = Environment.ProcessId, camera = Bounded(camera, 256), backend = Bounded(backend, 64), capture,
            correlation = $"{Environment.ProcessId}:nina:{capture}",
            timestampUnixUs = (DateTime.UtcNow.Ticks - DateTime.UnixEpoch.Ticks) / 10,
            details = new { api = Bounded(api, 64), outcome = Bounded(outcome, 32),
                elapsedUs = Stopwatch.GetElapsedTime(started).TotalMicroseconds,
                errorType = error is null ? null : Bounded(error.GetType().Name, 128), width, height, pixelCount }
        };
        return Parse(Bounded(backend, 64), Prefix + JsonSerializer.Serialize(record));
    }

    internal static void ApiTiming(string camera, string backend, long capture, string api, long started,
        string outcome, Exception? error = null, int? width = null, int? height = null, int? pixelCount = null)
    {
        if (!VerboseEnabled && error is null) return;
        try
        {
            Forward(ApiTimingEntry(camera, backend, capture, api, started, outcome, error, width, height, pixelCount),
                text => Logger.Info(text), text => Logger.Warning(text), text => Logger.Debug(text));
        }
        catch { /* Timing diagnostics cannot change API results or cancellation. */ }
    }

    internal static void Forward(Entry entry, Action<string> info, Action<string> warning, Action<string> debug)
    {
        if (entry.Suppressed) return;
        // Structured records always reach INFO, even when NINA's debug log is
        // disabled. Keep warning routing too, without allowing a failed sink
        // to suppress the other sink or affect a command/capture.
        if (entry.IncludeInfo) TryWrite(info, entry.Text);
        if (entry.IncludeInfo && entry.Severity is Level.Info or Level.Debug) return;
        TryWrite(entry.Severity switch { Level.Debug => debug, Level.Warning => warning, _ => info }, entry.Text);
    }

    private static void TryWrite(Action<string> sink, string text)
    {
        try
        {
            sink(text);
        }
        catch { /* A failed log sink must never fail a capture. */ }
    }
}
