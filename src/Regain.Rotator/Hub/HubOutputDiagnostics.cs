using System.Globalization;
using System.Text.Json;
using System.Text.RegularExpressions;

namespace Regain.Hub;

internal static class HubDiagnosticContract
{
    internal static bool Equal(JsonElement a, JsonElement b)
    {
        if (a.ValueKind != b.ValueKind) return false;
        if (a.ValueKind == JsonValueKind.Object) return a.EnumerateObject().Count() == b.EnumerateObject().Count() &&
            a.EnumerateObject().All(p => b.TryGetProperty(p.Name, out var value) && Equal(p.Value, value));
        if (a.ValueKind == JsonValueKind.Array) return a.GetArrayLength() == b.GetArrayLength() && a.EnumerateArray().Zip(b.EnumerateArray(), Equal).All(v => v);
        if (a.ValueKind == JsonValueKind.Number) return a.GetDouble() == b.GetDouble();
        if (a.ValueKind == JsonValueKind.String) return a.GetString() == b.GetString();
        return a.GetRawText() == b.GetRawText();
    }
    internal static void Schema(JsonElement schema, JsonElement value)
    {
        if (!Valid(schema, schema, value, 0)) throw new HubException(HubFailure.Protocol);
    }
    private static bool Valid(JsonElement root, JsonElement node, JsonElement value, int depth)
    {
        if (depth > 32) return false;
        if (node.TryGetProperty("$ref", out var reference)) {
            var key = reference.GetString()!;
            if (!key.StartsWith("#/$defs/", StringComparison.Ordinal) || !root.GetProperty("$defs").TryGetProperty(key.Substring(8).Replace("~1", "/").Replace("~0", "~"), out var target)) return false;
            if (!Valid(root, target, value, depth + 1)) return false;
        }
        if (node.TryGetProperty("oneOf", out var one) && one.EnumerateArray().Count(n => Valid(root, n, value, depth + 1)) != 1) return false;
        if (node.TryGetProperty("anyOf", out var any) && !any.EnumerateArray().Any(n => Valid(root, n, value, depth + 1))) return false;
        if (node.TryGetProperty("allOf", out var all) && !all.EnumerateArray().All(n => Valid(root, n, value, depth + 1))) return false;
        if (node.TryGetProperty("const", out var constant) && !Equal(constant, value)) return false;
        if (node.TryGetProperty("enum", out var choices) && !choices.EnumerateArray().Any(v => Equal(v, value))) return false;
        bool IsType(string? type) => type switch {
            "object" => value.ValueKind == JsonValueKind.Object, "array" => value.ValueKind == JsonValueKind.Array,
            "string" => value.ValueKind == JsonValueKind.String, "boolean" => value.ValueKind is JsonValueKind.True or JsonValueKind.False,
            "null" => value.ValueKind == JsonValueKind.Null, "integer" => value.ValueKind == JsonValueKind.Number && value.TryGetDecimal(out var number) && decimal.Truncate(number) == number,
            "number" => value.ValueKind == JsonValueKind.Number && value.TryGetDouble(out var number) && !double.IsNaN(number) && !double.IsInfinity(number), _ => false
        };
        if (node.TryGetProperty("type", out var types) && !(types.ValueKind == JsonValueKind.Array ? types.EnumerateArray().Any(t => IsType(t.GetString())) : IsType(types.GetString()))) return false;
        if (value.ValueKind == JsonValueKind.Number) {
            var number = value.GetDouble();
            if (double.IsNaN(number) || double.IsInfinity(number) || node.TryGetProperty("minimum", out var min) && number < min.GetDouble() || node.TryGetProperty("maximum", out var max) && number > max.GetDouble() || node.TryGetProperty("exclusiveMinimum", out var exclusive) && number <= exclusive.GetDouble()) return false;
        }
        if (value.ValueKind == JsonValueKind.String) {
            var text = value.GetString()!;
            if (node.TryGetProperty("format", out var format) && format.GetString() == "uuid" && (!Guid.TryParseExact(text, "D", out var id) || id == Guid.Empty)) return false;
            var count = text.Length - text.Count(char.IsLowSurrogate);
            if (node.TryGetProperty("minLength", out var min) && count < min.GetInt32() || node.TryGetProperty("maxLength", out var max) && count > max.GetInt32() || node.TryGetProperty("pattern", out var pattern) && !Regex.IsMatch(text, pattern.GetString()!, RegexOptions.CultureInvariant, TimeSpan.FromSeconds(1))) return false;
        }
        if (value.ValueKind == JsonValueKind.Array) {
            if (value.GetArrayLength() > 4096 || node.TryGetProperty("minItems", out var min) && value.GetArrayLength() < min.GetInt32() || node.TryGetProperty("maxItems", out var max) && value.GetArrayLength() > max.GetInt32()) return false;
            if (node.TryGetProperty("items", out var items) && !value.EnumerateArray().All(v => Valid(root, items, v, depth + 1))) return false;
        }
        if (value.ValueKind == JsonValueKind.Object) {
            if (value.EnumerateObject().Select(p => p.Name).Distinct(StringComparer.Ordinal).Count() != value.EnumerateObject().Count()) return false;
            if (node.TryGetProperty("required", out var required) && required.EnumerateArray().Any(n => !value.TryGetProperty(n.GetString()!, out _))) return false;
            node.TryGetProperty("properties", out var properties);
            foreach (var property in value.EnumerateObject()) {
                if (properties.ValueKind == JsonValueKind.Object && properties.TryGetProperty(property.Name, out var child)) {
                    if (!Valid(root, child, property.Value, depth + 1)) return false;
                } else if (node.TryGetProperty("additionalProperties", out var additional) && (additional.ValueKind == JsonValueKind.False || additional.ValueKind == JsonValueKind.Object && !Valid(root, additional, property.Value, depth + 1))) return false;
            }
        }
        return true;
    }
    internal static void Reply(JsonElement description, JsonElement saved, JsonElement output, JsonElement result, int start, int limit)
    {
        Schema(description.GetProperty("responseSchema"), result);
        void Require(bool condition) { if (!condition) throw new HubException(HubFailure.Protocol); }
        var revision = saved.GetProperty("revision").GetGuid(); var device = output.GetProperty("device"); var kind = device.GetProperty("kind").GetString();
        var type = kind switch { "safety" => "safetymonitor", "switch" => "switch", "weather" => "observingconditions", _ => "unsupported" };
        var total = result.GetProperty("total").GetInt32(); var end = Math.Min(start + limit, total); var diagnostics = result.GetProperty("diagnostics");
        Require(result.GetProperty("purpose").GetString() == "cachedDiagnostics" && result.GetProperty("output").GetGuid() == output.GetProperty("id").GetGuid() && result.GetProperty("configurationRevision").GetGuid() == revision && result.GetProperty("deviceType").GetString() == type && result.GetProperty("observedSeconds").GetDouble() >= 0 && result.GetProperty("start").GetInt32() == start && result.GetProperty("limit").GetInt32() == limit && total >= start && total <= 1024 && diagnostics.GetProperty("kind").GetString() == kind);
        Require(end < total ? result.GetProperty("nextStart").GetInt32() == end : result.GetProperty("nextStart").ValueKind == JsonValueKind.Null);
        void Health(JsonElement health) => Require(health.GetProperty("revision").GetGuid() == revision && saved.GetProperty("sources").EnumerateArray().Any(s => s.GetProperty("id").GetGuid() == health.GetProperty("source").GetGuid()));
        if (kind == "switch") {
            var active = device.GetProperty("channels").EnumerateArray().ToArray(); var numbers = active.Select(c => c.GetProperty("number").GetInt32()).ToList();
            if (saved.TryGetProperty("identities", out var ledger) && ledger.TryGetProperty("channels", out var channels)) foreach (var channel in channels.EnumerateObject().Select(p => p.Value)) if (channel.GetProperty("output").GetGuid() == output.GetProperty("id").GetGuid()) numbers.Add(channel.GetProperty("number").GetInt32());
            Require(total == (numbers.Count == 0 ? 0 : numbers.Max() + 1)); var items = diagnostics.GetProperty("channels"); Require(items.GetArrayLength() == end - start);
            for (int i = 0; i < items.GetArrayLength(); i++) {
                var item = items[i]; var channel = active.FirstOrDefault(c => c.GetProperty("number").GetInt32() == start + i);
                Require(item.GetProperty("number").GetInt32() == start + i && item.GetProperty("state").GetString() == (channel.ValueKind == JsonValueKind.Undefined ? "removed" : "configured"));
                if (channel.ValueKind == JsonValueKind.Undefined) continue;
                var readout = channel.GetProperty("readout"); Require(item.GetProperty("id").GetGuid() == channel.GetProperty("id").GetGuid() && Equal(item.GetProperty("readout"), readout) && item.GetProperty("configuredWritable").GetBoolean() == channel.GetProperty("writable").GetBoolean()); Health(item.GetProperty("health"));
                Require(item.GetProperty("health").GetProperty("source").GetGuid() == readout.GetProperty("source").GetGuid());
                var sample = item.GetProperty("sample"); if (sample.GetProperty("state").GetString() == "available") { var reading = sample.GetProperty("reading"); Require(reading.GetProperty("source").GetGuid() == readout.GetProperty("source").GetGuid() && reading.GetProperty("revision").GetGuid() == revision && reading.GetProperty("ageSeconds").GetDouble() >= 0); }
            }
        } else if (kind == "safety") {
            var members = device.GetProperty("members"); var items = diagnostics.GetProperty("members"); var active = diagnostics.GetProperty("controllerActive").GetBoolean();
            Require(total == members.GetArrayLength() && items.GetArrayLength() == end - start && (active || !diagnostics.GetProperty("isSafe").GetBoolean()));
            for (int i = 0; i < items.GetArrayLength(); i++) {
                var item = items[i]; var member = members[start + i]; var decision = item.GetProperty("decision");
                Require(item.GetProperty("source").GetGuid() == member.GetProperty("source").GetGuid() && item.GetProperty("enabled").GetBoolean() == member.GetProperty("enabled").GetBoolean() && (!member.TryGetProperty("policy", out var policy) || Equal(item.GetProperty("policy"), policy)) && item.GetProperty("enabled").GetBoolean() == (decision.ValueKind != JsonValueKind.Null));
                Health(item.GetProperty("health")); Require(item.GetProperty("health").GetProperty("source").GetGuid() == item.GetProperty("source").GetGuid());
                if (decision.ValueKind != JsonValueKind.Null) Require(decision.GetProperty("configurationRevision").GetGuid() == revision && (active || !decision.GetProperty("permitsSafe").GetBoolean() && decision.GetProperty("rawIsSafe").ValueKind == JsonValueKind.Null));
            }
        } else {
            var configured = device.GetProperty("measurements").EnumerateObject().OrderBy(p => p.Name, StringComparer.Ordinal).ToArray(); var items = diagnostics.GetProperty("measurements");
            Require(total == configured.Length && items.GetArrayLength() == end - start && diagnostics.GetProperty("averagePeriodHours").GetDouble() >= 0);
            for (int i = 0; i < items.GetArrayLength(); i++) {
                var item = items[i]; var metric = configured[start + i]; var references = metric.Value.GetProperty("sources");
                Require(item.GetProperty("metric").GetString() == metric.Name && Equal(item.GetProperty("configuration"), metric.Value) && item.GetProperty("sources").GetArrayLength() == references.GetArrayLength());
                for (int j = 0; j < references.GetArrayLength(); j++) { var health = item.GetProperty("sources")[j]; Health(health); Require(health.GetProperty("source").GetGuid() == references[j].GetProperty("source").GetGuid()); }
                var sample = item.GetProperty("sample"); if (sample.GetProperty("state").GetString() == "available") { var reading = sample.GetProperty("reading"); Require(reading.GetProperty("revision").GetGuid() == revision && reading.GetProperty("ageSeconds").GetDouble() >= 0 && references.EnumerateArray().Any(r => Equal(r, reading.GetProperty("readout")))); }
            }
        }
    }
}

public sealed partial class HubEditorSession
{
    public JsonElement? LastOutputObservation { get; private set; }
    public JsonElement OutputDiagnosticDescription => Description?.GetProperty("outputDiagnostics") ?? throw new InvalidOperationException("Reload the host description first");
    public int DiagnosticParameter(string key, string text)
    {
        Alive(); var field = OutputDiagnosticDescription.GetProperty("parameters").GetProperty(key);
        if (field.GetProperty("type").GetString() != "integer" || !int.TryParse(text, NumberStyles.Integer, CultureInfo.InvariantCulture, out var value) || value < field.GetProperty("minimum").GetInt32() || value > field.GetProperty("maximum").GetInt32()) throw new InvalidOperationException("Invalid " + field.GetProperty("label").GetString());
        return value;
    }
    public async Task<JsonElement> OutputStatusAsync(Guid output, int start, int limit, CancellationToken cancellation = default)
    {
        using var operation = Borrow(); await operations.WaitAsync(cancellation).ConfigureAwait(false); bool started = false;
        try {
            Ready(); var saved = SavedConfiguration!.Value;
            var selected = saved.GetProperty("outputs").EnumerateArray().FirstOrDefault(o => o.GetProperty("id").GetGuid() == output);
            if (selected.ValueKind == JsonValueKind.Undefined) throw new InvalidOperationException("Select a saved output; apply and reload before reading a new output");
            DiagnosticParameter("start", start.ToString(CultureInfo.InvariantCulture)); DiagnosticParameter("limit", limit.ToString(CultureInfo.InvariantCulture));
            var description = OutputDiagnosticDescription;
            if (description.GetProperty("purpose").GetString() != "cachedDiagnostics" || description.GetProperty("opensSource").GetBoolean() || description.GetProperty("writesEquipment").GetBoolean() || description.GetProperty("countsSafetyObservations").GetBoolean() || !description.GetProperty("requiresRevision").GetBoolean() || !description.GetProperty("deadlineSeconds").TryGetInt32(out var seconds) || seconds < 1 || seconds > 300) throw new HubException(HubFailure.Protocol);
            using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation, lifetime.Token); timer.CancelAfter(TimeSpan.FromSeconds(seconds));
            LastOutputObservation = null; started = true;
            var result = await Rpc(new { op = "outputStatus", output, expectedRevision = saved.GetProperty("revision").GetGuid(), start, limit }, timer.Token).ConfigureAwait(false); Alive();
            try { HubDiagnosticContract.Reply(description, saved, selected, result, start, limit); }
            catch { throw new HubException(HubFailure.Protocol); }
            LastOutputObservation = Observation("cachedOutputHealth", result); return result.Clone();
        } catch (HubException error) {
            if (started && (error.Failure is not (HubFailure.Remote or HubFailure.Busy or HubFailure.InvalidRequest) || error.Remote?.Code is "revisionConflict" or "disconnected" or "unavailable" or "timeout" or "uncertain")) { reviewed = null; State = HubEditorState.Uncertain; }
            throw;
        } catch (OperationCanceledException) { if (started) { reviewed = null; State = HubEditorState.Uncertain; } throw; }
        finally { operations.Release(); }
    }
}
