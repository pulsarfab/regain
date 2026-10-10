using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace Regain.Hub;

public sealed partial class HubEditorSession
{
    public JsonElement? LastImport { get; private set; }
    public JsonElement TransferDescription => Description?.GetProperty("configurationTransfer")
        ?? throw new InvalidOperationException("Reload the host description first");

    public async Task<string> ExportConfigurationAsync(CancellationToken cancellation = default)
    {
        using var operation = Borrow(); await operations.WaitAsync(cancellation).ConfigureAwait(false);
        try {
            Ready(); var description = TransferDescription; TransferContract(description);
            using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation, lifetime.Token); timer.CancelAfter(TimeSpan.FromSeconds(15));
            var document = await Rpc(new { op = description.GetProperty("exportOperation").GetString(), expectedRevision = Draft!.Revision }, timer.Token).ConfigureAwait(false);
            Alive(); HubDiagnosticContract.Schema(description.GetProperty("documentSchema"), document);
            var configuration = document.GetProperty("configuration");
            if (configuration.GetProperty("revision").GetGuid() != Draft.Revision || configuration.GetProperty("instanceId").GetGuid() != InstanceId ||
                BoundSources(configuration).Any() || !Ids(document, "credentialSources").SetEquals(BoundSources(SavedConfiguration!.Value)))
                throw new HubException(HubFailure.Protocol);
            return JsonSerializer.Serialize(document, new JsonSerializerOptions { WriteIndented = true });
        } finally { operations.Release(); }
    }
    public async Task<JsonElement> PrepareImportAsync(string document, string mode, CancellationToken cancellation = default)
    {
        using var operation = Borrow(); await operations.WaitAsync(cancellation).ConfigureAwait(false);
        var started = false;
        try {
            Ready(); var description = TransferDescription; TransferContract(description);
            if (Encoding.UTF8.GetByteCount(document) > description.GetProperty("maximumDocumentBytes").GetInt32() || mode is not ("restore" or "copy"))
                throw new InvalidOperationException("Invalid configuration file or import mode");
            var version = Draft!.Version; var revision = Draft.Revision;
            using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation, lifetime.Token); timer.CancelAfter(TimeSpan.FromSeconds(15));
            started = true;
            var result = await Rpc(new { op = description.GetProperty("importOperation").GetString(), expectedRevision = revision, document, mode }, timer.Token).ConfigureAwait(false);
            Alive();
            ValidateImport(description, result, document, mode);
            if (Draft.Version != version) throw new InvalidOperationException("The draft changed during import preparation. Select the file again after reviewing the current draft");
            Draft.ReplaceImported(result.GetProperty("candidate"));
            LastImport = result.Clone(); Changed(); return result.Clone();
        } catch (HubException error) {
            if (started && (error.Failure is not (HubFailure.Remote or HubFailure.Busy or HubFailure.InvalidRequest) || error.Remote?.Code is "revisionConflict" or "disconnected")) {
                reviewed = null; State = HubEditorState.Uncertain;
            }
            throw;
        } catch (OperationCanceledException) { if (started) { reviewed = null; State = HubEditorState.Uncertain; } throw; }
        finally { operations.Release(); }
    }
    private static void TransferContract(JsonElement description)
    {
        if (description.GetProperty("persistsConfiguration").GetBoolean() || description.GetProperty("opensSource").GetBoolean() ||
            description.GetProperty("writesEquipment").GetBoolean() || description.GetProperty("formatVersion").GetInt32() != 1 ||
            description.GetProperty("exportOperation").GetString() != "exportConfig" || description.GetProperty("importOperation").GetString() != "prepareImport" ||
            !description.GetProperty("maximumDocumentBytes").TryGetInt32(out var maximum) || maximum < 1 || maximum > 4194304)
            throw new HubException(HubFailure.Protocol);
    }
    private static HashSet<Guid> Ids(JsonElement value, string property)
    {
        var result = new HashSet<Guid>();
        foreach (var element in value.GetProperty(property).EnumerateArray()) if (!result.Add(element.GetGuid())) throw new HubException(HubFailure.Protocol);
        return result;
    }
    private static IEnumerable<Guid> BoundSources(JsonElement config) => config.GetProperty("sources").EnumerateArray()
        .Where(s => s.GetProperty("backend").GetProperty("kind").GetString() == "alpaca" &&
            s.GetProperty("backend").TryGetProperty("credentialReference", out var reference) && reference.ValueKind != JsonValueKind.Null)
        .Select(s => s.GetProperty("id").GetGuid());
    private void ValidateImport(JsonElement description, JsonElement result, string text, string mode)
    {
        try {
            HubDiagnosticContract.Schema(description.GetProperty("responseSchema"), result);
            var candidate = result.GetProperty("candidate"); HubDiagnosticContract.Schema(Description!.Value.GetProperty("schema"), candidate);
            using var input = JsonDocument.Parse(text.TrimStart('\uFEFF'));
            var saved = SavedConfiguration!.Value;
            if (result.GetProperty("configurationRevision").GetGuid() != Draft!.Revision || result.GetProperty("mode").GetString() != mode ||
                result.GetProperty("sourceInstanceId").GetGuid() != input.RootElement.GetProperty("configuration").GetProperty("instanceId").GetGuid() ||
                candidate.GetProperty("revision").GetGuid() != Draft.Revision || candidate.GetProperty("instanceId").GetGuid() != InstanceId ||
                !HubDiagnosticContract.Equal(candidate.GetProperty("identities"), saved.GetProperty("identities"))) throw new FormatException();
            var preserved = Ids(result, "preservedCredentials"); var missing = Ids(result, "missingCredentials");
            if (missing.Overlaps(preserved) || !preserved.SetEquals(BoundSources(candidate))) throw new FormatException();
            foreach (var id in preserved.Concat(missing)) {
                var source = candidate.GetProperty("sources").EnumerateArray().Single(s => s.GetProperty("id").GetGuid() == id);
                if (source.GetProperty("backend").GetProperty("kind").GetString() != "alpaca") throw new FormatException();
                if (!preserved.Contains(id)) continue;
                var existing = saved.GetProperty("sources").EnumerateArray().Single(s => s.GetProperty("id").GetGuid() == id);
                if (mode != "restore" || !HubDiagnosticContract.Equal(source.GetProperty("backend").GetProperty("credentialReference"),
                    existing.GetProperty("backend").GetProperty("credentialReference"))) throw new FormatException();
            }
            var mappings = result.GetProperty("remappedIds").EnumerateArray().ToArray();
            if (mode == "restore" && (result.GetProperty("sourceInstanceId").GetGuid() != InstanceId || mappings.Length != 0 || result.GetProperty("renumberedOutputs").GetArrayLength() != 0) ||
                mode == "copy" && (preserved.Count != 0 || mappings.Select(m => m.GetProperty("original").GetGuid()).Distinct().Count() != mappings.Length ||
                    mappings.Select(m => m.GetProperty("replacement").GetGuid()).Distinct().Count() != mappings.Length ||
                    mappings.Any(m => m.GetProperty("original").GetGuid() == m.GetProperty("replacement").GetGuid()))) throw new FormatException();
        } catch { throw new HubException(HubFailure.Protocol); }
    }
    public static string ImportSummary(JsonElement result) => string.Join("\n", new[] {
        "Imported draft (" + result.GetProperty("mode").GetString() + "). Saved configuration is unchanged.",
        "New IDs: " + result.GetProperty("remappedIds").GetArrayLength() + ". Changed device numbers: " + result.GetProperty("renumberedOutputs").GetArrayLength() + ".",
        string.Join("\n", result.GetProperty("renumberedOutputs").EnumerateArray().Select(n => n.GetProperty("deviceType").GetString() + " " + n.GetProperty("output").GetString() + ": " + n.GetProperty("original").GetUInt32() + " → " + n.GetProperty("replacement").GetUInt32())),
        "Retained local credential bindings: " + result.GetProperty("preservedCredentials").GetArrayLength() + ".",
        result.GetProperty("missingCredentials").GetArrayLength() == 0 ? "" : "Supply credential bindings for sources:\n" + string.Join("\n", result.GetProperty("missingCredentials").EnumerateArray().Select(id => id.GetString())),
        "Review the draft and Apply separately." }.Where(line => line.Length > 0));
}

public sealed partial class HubConfigurationDraft
{
    internal void ReplaceImported(JsonElement candidate)
    {
        HubDiagnosticContract.Schema(Description.Root, candidate);
        foreach (var key in new[] { "instanceId", "revision", "identities", "schemaVersion" })
            if (!HubDiagnosticContract.Equal(candidate.GetProperty(key), Candidate.GetProperty(key))) throw new HubException(HubFailure.Protocol);
        var replacement = JsonNode.Parse(candidate.GetRawText())!.AsObject();
        // Keep the saved baseline for Dirty and immutable-field behavior.
        value.Clear(); foreach (var property in replacement.ToArray()) value.Add(property.Key, property.Value?.DeepClone());
        Version++;
    }
}
