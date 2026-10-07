using System.Globalization;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace Regain.Hub;

/// Test controls are described by the host, including sparse nested updates.
public sealed class HubSimulationControl
{
    public JsonElement Descriptor { get; }
    public string[] Path { get; }
    public string Label => Descriptor.GetProperty("label").GetString()!;
    public string Type => Descriptor.GetProperty("type").GetString()!;
    public JsonElement Default => Descriptor.GetProperty("default");
    internal HubSimulationControl(JsonElement descriptor)
    {
        Descriptor = descriptor.Clone(); Path = descriptor.GetProperty("path").EnumerateArray().Select(p => p.GetString()!).ToArray();
        if (Path.Length is < 1 or > 2 || Path.Any(p => string.IsNullOrEmpty(p) || p.Any(c => !char.IsLetterOrDigit(c))))
            throw new HubException(HubFailure.Protocol);
    }
    public JsonElement Parse(string text, bool absent = false)
    {
        JsonElement value;
        if (absent) value = JsonSerializer.SerializeToElement<object?>(null);
        else if (Type == "integer") {
            if (!int.TryParse(text, NumberStyles.AllowLeadingSign, CultureInfo.InvariantCulture, out var integer))
                throw new InvalidOperationException("Invalid " + Label);
            value = JsonSerializer.SerializeToElement(integer);
        } else if (Type == "number") {
            if (!double.TryParse(text, NumberStyles.Float, CultureInfo.InvariantCulture, out var number) || double.IsNaN(number) || double.IsInfinity(number))
                throw new InvalidOperationException("Invalid " + Label);
            value = JsonSerializer.SerializeToElement(number);
        } else if (Type == "boolean") {
            if (!bool.TryParse(text, out var flag)) throw new InvalidOperationException("Invalid " + Label);
            value = JsonSerializer.SerializeToElement(flag);
        } else if (Type is "strings" or "integers") {
            try { using var document = JsonDocument.Parse(text); value = document.RootElement.Clone(); }
            catch (JsonException) { throw new InvalidOperationException("Invalid " + Label); }
        } else value = JsonSerializer.SerializeToElement(text);
        Validate(value); return value;
    }
    public void Validate(JsonElement value)
    {
        if (value.ValueKind == JsonValueKind.Null && Descriptor.TryGetProperty("nullable", out var nullable) && nullable.ValueKind == JsonValueKind.True) return;
        bool valid = Type switch {
            "boolean" => value.ValueKind is JsonValueKind.True or JsonValueKind.False,
            "string" => value.ValueKind == JsonValueKind.String && Descriptor.GetProperty("enum").EnumerateArray().Any(c => c.GetString() == value.GetString()),
            "integer" => value.ValueKind == JsonValueKind.Number && value.TryGetInt32(out var integer) &&
                (!Descriptor.TryGetProperty("minimum", out var minInteger) || integer >= minInteger.GetInt32()) &&
                (!Descriptor.TryGetProperty("maximum", out var maxInteger) || integer <= maxInteger.GetInt32()),
            "number" => value.ValueKind == JsonValueKind.Number && value.TryGetDouble(out var number) && !double.IsNaN(number) && !double.IsInfinity(number) &&
                NumberBounds(number) && (!Descriptor.TryGetProperty("precision", out var precision)
                    || precision.GetString() == "single" && NumberBounds((float)number)),
            "strings" or "integers" => ArrayBounds(value),
            _ => false
        };
        if (!valid) throw new InvalidOperationException("Invalid " + Label);
    }
    private bool ArrayBounds(JsonElement value)
    {
        if (value.ValueKind != JsonValueKind.Array || value.GetArrayLength() < Descriptor.GetProperty("minItems").GetInt32()
            || value.GetArrayLength() > Descriptor.GetProperty("maxItems").GetInt32()) return false;
        try {
            long bytes = 0; var reference = false;
            foreach (var item in value.EnumerateArray()) {
                if (Type == "integers") {
                    if (item.ValueKind != JsonValueKind.Number || !item.TryGetInt32(out var integer) || !NumberBounds(integer)) return false;
                    reference |= Descriptor.TryGetProperty("contains", out var required) && integer == required.GetInt32();
                } else {
                    if (item.ValueKind != JsonValueKind.String) return false;
                    bytes += new System.Text.UTF8Encoding(false,true).GetByteCount(item.GetString()!);
                    if (bytes > Descriptor.GetProperty("maxUtf8Bytes").GetInt32()) return false;
                }
            }
            return !Descriptor.TryGetProperty("contains",out _) || reference;
        } catch (ArgumentException) { return false; }
    }
    private bool NumberBounds(double number) => !double.IsNaN(number) && !double.IsInfinity(number) &&
        (!Descriptor.TryGetProperty("minimum", out var minimum) || number >= minimum.GetDouble()) &&
        (!Descriptor.TryGetProperty("maximum", out var maximum) || number <= maximum.GetDouble()) &&
        (!Descriptor.TryGetProperty("exclusiveMinimum", out var exclusiveMinimum) || number > exclusiveMinimum.GetDouble()) &&
        (!Descriptor.TryGetProperty("exclusiveMaximum", out var exclusiveMaximum) || number < exclusiveMaximum.GetDouble());
    public JsonElement Read(JsonElement status)
    {
        var value = status;
        foreach (var key in Path) value = value.GetProperty(key);
        Validate(value); return value.Clone();
    }
}

public sealed partial class HubEditorSession
{
    public JsonElement SimulationDescription => Description?.GetProperty("simulationControl") ?? throw new InvalidOperationException("Reload the host description first");
    public IReadOnlyList<HubSimulationControl> SimulationControls(Guid source)
    {
        var backend = SavedSource(source).GetProperty("backend"); var description = SimulationDescription;
        if (!description.GetProperty("sourceKinds").EnumerateArray().Any(k => k.GetString() == backend.GetProperty("kind").GetString()))
            throw new InvalidOperationException("Select an explicitly simulated source");
        var type = backend.GetProperty("deviceType").GetString()!;
        var fields = description.GetProperty("controlsByDeviceType").GetProperty(type);
        if (fields.GetArrayLength() is < 1 or > 32 || description.GetProperty("revisionCheckedUpdates").ValueKind != JsonValueKind.True)
            throw new HubException(HubFailure.Protocol);
        var result = fields.EnumerateArray().Select(f => new HubSimulationControl(f)).ToArray();
        if (result.Select(f => string.Join("/", f.Path)).Distinct(StringComparer.Ordinal).Count() != result.Length) throw new HubException(HubFailure.Protocol);
        return result;
    }
    private void ValidateSimulationPatch(Guid source, JsonElement update)
    {
        var controls = SimulationControls(source);
        if (update.ValueKind != JsonValueKind.Object || !update.EnumerateObject().Any()) throw new InvalidOperationException("Select at least one simulation field to change");
        var seen = new HashSet<string>(StringComparer.Ordinal);
        void Check(string[] path, JsonElement value) {
            var key = string.Join("/", path);
            if (!seen.Add(key)) throw new InvalidOperationException("Duplicate simulation field");
            var control = controls.SingleOrDefault(c => c.Path.SequenceEqual(path)) ?? throw new InvalidOperationException("Unknown simulation field");
            control.Validate(value);
        }
        foreach (var field in update.EnumerateObject()) {
            if (field.Value.ValueKind == JsonValueKind.Object) {
                if (!field.Value.EnumerateObject().Any()) throw new InvalidOperationException("Select a simulation reading to change");
                foreach (var child in field.Value.EnumerateObject()) Check([field.Name, child.Name], child.Value);
            } else Check([field.Name], field.Value);
        }
    }
    public async Task<JsonElement> UpdateSimulationAsync(Guid source, JsonElement update, CancellationToken cancellation = default)
    {
        using var operation = Borrow(); await operations.WaitAsync(cancellation).ConfigureAwait(false); var started = false;
        try {
            Ready(); ValidateSimulationPatch(source, update);
            var seconds = SimulationDescription.GetProperty("deadlineSeconds").GetInt32();
            if (seconds is < 1 or > 300) throw new HubException(HubFailure.Protocol);
            reviewed = null; State = HubEditorState.Editing; LastSourceObservation = null;
            using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation, lifetime.Token); timer.CancelAfter(TimeSpan.FromSeconds(seconds + 5)); started = true;
            var result = await Rpc(new { op = "updateSimulation", source, expectedRevision = Draft!.Revision, update }, timer.Token).ConfigureAwait(false);
            Alive();
            try {
                HubWire.Members(result, "source", "configurationRevision", "simulation");
                if (result.GetProperty("source").GetGuid() != source || result.GetProperty("configurationRevision").GetGuid() != Draft!.Revision) throw new FormatException();
                ValidateSimulationStatus(source, result.GetProperty("simulation"));
            } catch { throw new HubException(HubFailure.Protocol); }
            LastSourceObservation = Observation("simulationUpdate", result); return result.Clone();
        } catch (HubException error) {
            if (started && (error.Failure is not (HubFailure.Remote or HubFailure.Busy or HubFailure.InvalidRequest) ||
                error.Remote?.Code is "uncertain" or "revisionConflict" or "disconnected" or "unavailable" or "timeout" or "transient" or "responseTooLarge")) {
                reviewed = null; State = HubEditorState.Uncertain;
            } throw;
        } catch (OperationCanceledException) { if (started) { reviewed = null; State = HubEditorState.Uncertain; } throw; }
        finally { operations.Release(); }
    }
    public void ValidateSimulationStatus(Guid source, JsonElement status)
    {
        var type = SavedSource(source).GetProperty("backend").GetProperty("deviceType").GetString();
        var controls = SimulationControls(source);
        string[] baseline = ["deviceType", "safe", "switchValues", "weather", "fault", "sampleAgeSeconds"];
        HubWire.Members(status, baseline.Concat(controls.Where(control => control.Path.Length == 2)
            .Select(control => control.Path[0])).Distinct(StringComparer.Ordinal).ToArray());
        if (status.GetProperty("deviceType").GetString() != type) throw new HubException(HubFailure.Protocol);
        foreach (var group in controls.Where(control => control.Path.Length == 2).GroupBy(control => control.Path[0]))
            HubWire.Members(status.GetProperty(group.Key), group.Select(control => control.Path[1]).ToArray());
        foreach (var control in controls) control.Read(status);
        if (status.TryGetProperty("filterWheel",out var wheel)) {
            var metadata = HubFilterWheelProtocol.Metadata(wheel.GetProperty("names"),wheel.GetProperty("focusOffsets"));
            if (wheel.GetProperty("position").GetInt32() >= metadata.Names.Length) throw new HubException(HubFailure.Protocol);
        }
        if (status.TryGetProperty("coverCalibrator", out var panel)) {
            var brightness = panel.GetProperty("brightness").GetInt32();
            var cover = panel.GetProperty("coverState").GetInt32();
            var light = panel.GetProperty("calibratorState").GetInt32();
            if (brightness > panel.GetProperty("maxBrightness").GetInt32()
                || light <= 1 && brightness != 0
                || cover == 0 && panel.GetProperty("coverMoving").GetBoolean()
                || light == 0 && panel.GetProperty("calibratorChanging").GetBoolean()) throw new HubException(HubFailure.Protocol);
        }
    }
    internal static JsonElement SimulationPatch(IEnumerable<KeyValuePair<HubSimulationControl, JsonElement>> selected)
    {
        var patch = new JsonObject();
        foreach (var field in selected) {
            field.Key.Validate(field.Value); var path = field.Key.Path;
            if (path.Length == 1) patch.Add(path[0], JsonNode.Parse(field.Value.GetRawText()));
            else {
                if (!patch.ContainsKey(path[0])) patch[path[0]] = new JsonObject();
                patch[path[0]]!.AsObject().Add(path[1], JsonNode.Parse(field.Value.GetRawText()));
            }
        }
        return JsonSerializer.SerializeToElement(patch);
    }
}
