using System.Text.Json;
using NINA.Core.Utility;

namespace ZwoGain.NINA;

/// <summary>Worker stderr is diagnostic feedback, never an operator notification.</summary>
internal static class CameraLog
{
    internal enum Level { Debug, Info, Warning }
    internal sealed record Entry(Level Severity, string Text);
    private const string Prefix = "ZWOGAIN_DIAGNOSTIC ";

    internal static Entry Parse(string backend, string line)
    {
        // Bound legacy/unstructured output too (including SDK loader failures).
        if (line.Length > 8192) line = line[..8192] + " [truncated]";
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
                    string process = root.TryGetProperty("pid", out var pid) && pid.TryGetInt32(out var id) ? $" worker {id}" : "";
                    return new(severity, Clean($"ZWOgain {backend}{process} [{kind.GetString()}]: {message.GetString()}"));
                }
            }
            catch (JsonException) { /* Keep malformed diagnostics visible as plain text. */ }
            catch (InvalidOperationException) { /* Wrong JSON field types are not camera failures. */ }
        }
        return new(Level.Info, Clean($"ZWOgain {backend}: {line}"));
    }

    private static string Clean(string text) => text.Replace('\r', ' ').Replace('\n', ' ');

    internal static void Worker(string backend, string line) => Forward(Parse(backend, line),
        text => Logger.Info(text), text => Logger.Warning(text), text => Logger.Debug(text));

    internal static void Session(string camera, string message) => Forward(new(
        message is "Idle" or "Starting exposure" or "Exposing" or "Downloading" ? Level.Debug : Level.Info,
        Clean($"ZWOgain {camera}: {message}")),
        text => Logger.Info(text), text => Logger.Warning(text), text => Logger.Debug(text));

    internal static void Forward(Entry entry, Action<string> info, Action<string> warning, Action<string> debug)
    {
        try
        {
            switch (entry.Severity)
            {
                case Level.Debug: debug(entry.Text); break;
                case Level.Warning: warning(entry.Text); break;
                default: info(entry.Text); break;
            }
        }
        catch { /* A failed log sink must never fail a capture. */ }
    }
}
