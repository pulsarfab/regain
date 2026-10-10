using System.Text.Json;

namespace Regain.Hub;

public sealed partial class HubEditorSession
{
    public JsonElement? LastLocalDiscovery { get; private set; }
    public JsonElement LocalDiscoveryDescription => Description?.GetProperty("discovery").GetProperty("local")
        ?? throw new InvalidOperationException("Reload the host description first");

    public async Task<JsonElement> DiscoverLocalAsync(JsonElement target, CancellationToken cancellation = default)
    {
        using var operation = Borrow();
        await operations.WaitAsync(cancellation).ConfigureAwait(false);
        var started = false;
        try {
            Ready(); var description = LocalDiscoveryDescription;
            HubDiagnosticContract.Schema(description.GetProperty("targetSchema"), target);
            if (description.GetProperty("opensSource").GetBoolean() || description.GetProperty("writesEquipment").GetBoolean() ||
                description.GetProperty("persistsConfiguration").GetBoolean() ||
                !description.GetProperty("timeoutSeconds").TryGetInt32(out var seconds) || seconds < 1 || seconds > 300)
                throw new HubException(HubFailure.Protocol);
            var capability = description.GetProperty(target.GetProperty("kind").GetString()!).GetProperty("requiresCapability").GetString();
            if (!Description!.Value.GetProperty("capabilities").EnumerateArray().Any(c => c.GetString() == capability))
                throw new InvalidOperationException("This discovery backend is unavailable in the host");
            var revision = Draft!.Revision; LastLocalDiscovery = null;
            using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation, lifetime.Token);
            timer.CancelAfter(TimeSpan.FromSeconds(seconds + 5)); started = true;
            var result = await Rpc(new { op = description.GetProperty("operation").GetString(), target, expectedRevision = revision }, timer.Token).ConfigureAwait(false);
            Alive();
            try {
                HubLocalCatalog.Validate(description, result, target, revision);
                if (Draft.Revision != revision) throw new FormatException();
            } catch { throw new HubException(HubFailure.Protocol); }
            LastLocalDiscovery = result.Clone(); return result.Clone();
        } catch (HubException error) {
            if (started && (error.Failure is not (HubFailure.Remote or HubFailure.Busy or HubFailure.InvalidRequest) ||
                error.Remote?.Code is "revisionConflict" or "disconnected" or "invalidValue")) {
                reviewed = null; State = HubEditorState.Uncertain;
            }
            throw;
        } catch (OperationCanceledException) {
            if (started) { reviewed = null; State = HubEditorState.Uncertain; } throw;
        } finally { operations.Release(); }
    }
    public Guid AddDiscoveredLocalSource(int index)
    {
        Ready();
        if (operations.CurrentCount == 0 || LastLocalDiscovery is not JsonElement catalog)
            throw new InvalidOperationException("Finish a catalog query before selecting a device");
        var id = Draft!.AddDiscoveredLocalSource(catalog, index); Changed(); return id;
    }
}

internal static class HubLocalCatalog
{
    internal static void Validate(JsonElement description, JsonElement result, JsonElement target, Guid revision)
    {
        HubDiagnosticContract.Schema(description.GetProperty("responseSchema"), result);
        var returned = result.GetProperty("target"); var kind = target.GetProperty("kind").GetString()!;
        if (result.GetProperty("configurationRevision").GetGuid() != revision || returned.GetProperty("kind").GetString() != kind ||
            (kind == "native" ? returned.GetProperty("device").GetString() != target.GetProperty("device").GetString() :
                returned.GetProperty("deviceType").GetString() != target.GetProperty("deviceType").GetString() || returned.GetProperty("bitness").GetString() != target.GetProperty("bitness").GetString()) ||
            kind == "com" && result.GetProperty("simulated").GetBoolean() ||
            result.GetProperty("ignoredEntries").GetUInt32() != 0 && !result.GetProperty("incomplete").GetBoolean()) throw new FormatException();
        var identities = new HashSet<string>(StringComparer.Ordinal);
        foreach (var entry in result.GetProperty("entries").EnumerateArray()) {
            var backend = entry.GetProperty("backend"); var name = entry.GetProperty("name").GetString()!;
            var registered = entry.GetProperty("registeredClass"); var blocked = entry.GetProperty("blockedReason");
            if (string.IsNullOrWhiteSpace(name) || name.Any(char.IsControl) || backend.GetProperty("kind").GetString() != kind) throw new FormatException();
            string identity;
            if (kind == "native") {
                var device = target.GetProperty("device").GetString()!; var serial = backend.GetProperty("identity").GetString()!;
                if (backend.GetProperty("device").GetString() != device || string.IsNullOrWhiteSpace(serial) || serial.Any(char.IsControl) ||
                    registered.ValueKind != JsonValueKind.Null || blocked.ValueKind != JsonValueKind.Null ||
                    backend.TryGetProperty("filterWheel", out var wheel) && wheel.ValueKind != JsonValueKind.Null) throw new FormatException();
                var camera = backend.TryGetProperty("camera", out var settings) ? settings : default;
                if (device is "camera-direct" or "camera-sdk") {
                    if (camera.ValueKind != JsonValueKind.Object || camera.GetProperty("model").GetString() != name || camera.GetProperty("sdkFallback").GetBoolean()) throw new FormatException();
                } else if (camera.ValueKind is not (JsonValueKind.Null or JsonValueKind.Undefined)) throw new FormatException();
                identity = serial.ToLowerInvariant();
                if (device == "camera-direct" && result.GetProperty("simulated").GetBoolean()) identity += ":" + name;
            } else {
                identity = backend.GetProperty("progId").GetString()!;
                if (identity.Length == 0 || identity.Any(c => !(c is >= 'A' and <= 'Z' or >= 'a' and <= 'z' or >= '0' and <= '9' or '.' or '_' or '-')) ||
                    backend.GetProperty("deviceType").GetString() != target.GetProperty("deviceType").GetString() ||
                    backend.GetProperty("bitness").GetString() != target.GetProperty("bitness").GetString() ||
                    backend.GetProperty("connectionPolicy").GetString() != "externallyManaged" ||
                    (registered.ValueKind == JsonValueKind.Null ? blocked.GetString() != "missingRegistration" :
                        registered.GetGuid() == Guid.Empty || blocked.ValueKind != JsonValueKind.Null && blocked.GetString() != "selfProxy")) throw new FormatException();
                identity = identity.ToLowerInvariant();
            }
            if (!identities.Add(identity)) throw new FormatException();
        }
    }
}
