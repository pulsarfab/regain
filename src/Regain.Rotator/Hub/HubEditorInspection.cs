using System.Globalization;
using System.Text.Json;

namespace Regain.Hub;

public sealed partial class HubEditorSession
{
    public JsonElement? LastSourceObservation { get; private set; }
    public JsonElement InspectionDescription => Description?.GetProperty("capabilityInspection")
        ?? throw new InvalidOperationException("Reload the host description first");
    private JsonElement SavedSource(Guid source)
    {
        Alive();
        if (source == Guid.Empty || SavedConfiguration is not JsonElement saved)
            throw new InvalidOperationException("Select a saved source after reloading");
        foreach (var entry in saved.GetProperty("sources").EnumerateArray())
            if (entry.GetProperty("id").GetGuid() == source) return entry;
        throw new InvalidOperationException("Apply and reload before inspecting a new source");
    }
    // Bounds/defaults are host descriptors, shared by every setup frontend.
    public int InspectionParameter(string key, string text)
    {
        Alive(); var field = InspectionDescription.GetProperty("parameters").GetProperty(key);
        if (field.GetProperty("type").GetString() != "integer" ||
            !int.TryParse(text, NumberStyles.Integer, CultureInfo.InvariantCulture, out var value) ||
            value < field.GetProperty("minimum").GetInt32() || value > field.GetProperty("maximum").GetInt32())
            throw new InvalidOperationException("Invalid " + field.GetProperty("label").GetString());
        return value;
    }
    public async Task<JsonElement> InspectSourceAsync(Guid source, int start, int limit, CancellationToken cancellation = default)
    {
        using var operation = Borrow();
        await operations.WaitAsync(cancellation).ConfigureAwait(false);
        var started = false;
        try {
            Ready(); SavedSource(source);
            start = InspectionParameter("start", start.ToString(CultureInfo.InvariantCulture));
            limit = InspectionParameter("limit", limit.ToString(CultureInfo.InvariantCulture));
            var description = InspectionDescription;
            if (description.GetProperty("purpose").GetString() != "setupOnly" ||
                !description.GetProperty("opensSource").GetBoolean() || description.GetProperty("writesEquipment").GetBoolean() ||
                !description.GetProperty("deadlineSeconds").TryGetInt32(out var seconds) || seconds < 1 || seconds > 300)
                throw new HubException(HubFailure.Protocol);
            reviewed = null; State = HubEditorState.Editing; LastSourceObservation = null;
            using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation, lifetime.Token);
            // Allow the host's bounded probe to finish cleanup and return its error.
            timer.CancelAfter(TimeSpan.FromSeconds(seconds + 5)); started = true;
            var result = await Rpc(new { op = "inspectSource", source, start, limit }, timer.Token).ConfigureAwait(false);
            Alive();
            try {
                HubWire.Members(result, "purpose", "source", "configurationRevision", "generation", "deviceType", "connection",
                    "simulation", "startedSeconds", "completedSeconds", "capabilities");
                if (result.GetProperty("purpose").GetString() != "setupOnly" || result.GetProperty("source").GetGuid() != source ||
                    result.GetProperty("configurationRevision").GetGuid() != Draft!.Revision || result.GetProperty("generation").GetGuid() == Guid.Empty ||
                    result.GetProperty("capabilities").ValueKind != JsonValueKind.Object)
                    throw new FormatException();
                var began = result.GetProperty("startedSeconds").GetDouble(); var ended = result.GetProperty("completedSeconds").GetDouble();
                if (double.IsNaN(began) || double.IsInfinity(began) || began < 0 || double.IsNaN(ended) || double.IsInfinity(ended) || ended < began)
                    throw new FormatException();
                var capabilities = result.GetProperty("capabilities");
                if (capabilities.TryGetProperty("nextStart", out var cursor) && cursor.ValueKind != JsonValueKind.Null) {
                    var next = cursor.GetInt32();
                    InspectionParameter("start", next.ToString(CultureInfo.InvariantCulture));
                    if (capabilities.GetProperty("kind").GetString() != "switch" || next != start + limit) throw new FormatException();
                }
            } catch { throw new HubException(HubFailure.Protocol); }
            LastSourceObservation = Observation("setupInspection", result);
            return result.Clone();
        } catch (HubException error) {
            if (started && (error.Failure is not (HubFailure.Remote or HubFailure.Busy or HubFailure.InvalidRequest) ||
                error.Remote?.Code is "revisionConflict" or "disconnected")) { reviewed = null; State = HubEditorState.Uncertain; }
            throw;
        } catch (OperationCanceledException) {
            if (started) { reviewed = null; State = HubEditorState.Uncertain; } throw;
        } finally { operations.Release(); }
    }
    private JsonElement Observation(string kind, JsonElement result) => JsonSerializer.SerializeToElement(new {
        kind, observedAtUtc = DateTimeOffset.UtcNow, instanceId = InstanceId, configurationRevision = Draft!.Revision, result
    });
    // Only setup observations and public host status are exported. Neither the
    // editable configuration nor credential requests are collected here.
    public JsonElement DiagnosticSnapshot()
    {
        Alive();
        return JsonSerializer.SerializeToElement(new { format = "regainHubSetupDiagnostics", version = 1,
            exportedAtUtc = DateTimeOffset.UtcNow, instanceId = InstanceId, editorState = State.ToString(),
            savedRevision = SavedConfiguration?.GetProperty("revision"), savedHostStatus = HostStatus,
            observation = LastSourceObservation });
    }
}
