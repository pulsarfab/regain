using System.Text.Json;

namespace Regain.Hub;

public sealed partial class HubEditorSession
{
    public JsonElement? CredentialDescription => Description is JsonElement description &&
        description.TryGetProperty("credentialStorage", out var storage) && storage.ValueKind == JsonValueKind.Object ? storage : null;
    public string CredentialReference(Guid id)
    {
        Alive();
        if (id == Guid.Empty || CredentialDescription is not JsonElement storage ||
            !storage.TryGetProperty("clientChosenReferences", out var named) || named.ValueKind != JsonValueKind.True)
            throw new InvalidOperationException("This host does not support recoverable credential creation");
        return storage.GetProperty("referencePrefix").GetString() + id.ToString("D");
    }
    public Task<JsonElement> CreateCredentialAsync(Guid referenceId, string authorization, CancellationToken cancellation = default)
    {
        var expected = CredentialReference(referenceId);
        var input = CredentialDescription!.Value.GetProperty("input").GetProperty("authorization");
        if (!input.GetProperty("writeOnly").GetBoolean() || !input.GetProperty("sensitive").GetBoolean() ||
            string.IsNullOrWhiteSpace(authorization) || authorization.Length > input.GetProperty("maxLength").GetInt32() ||
            authorization.Any(c => c > 127 || char.IsControl(c))) throw new InvalidOperationException("Invalid authorization value");
        return CredentialOperation(new { op = "createCredential", referenceId, authorization }, true, expected, false, cancellation);
    }
    public Task<JsonElement> CredentialStatusAsync(string reference, CancellationToken cancellation = default)
    {
        ValidateReference(reference);
        return CredentialOperation(new { op = "credentialStatus", reference }, false, reference, false, cancellation);
    }
    public Task<JsonElement> DeleteCredentialAsync(string reference, CancellationToken cancellation = default)
    {
        ValidateReference(reference);
        return CredentialOperation(new { op = "deleteCredential", reference }, true, reference, true, cancellation);
    }
    private void ValidateReference(string reference)
    {
        Alive();
        if (CredentialDescription is not JsonElement storage) throw new InvalidOperationException("Credential storage is unavailable on this host");
        var schema = storage.GetProperty("reference");
        var length = 0;
        for (int i = 0; i < reference.Length; i++, length++) {
            if (char.IsHighSurrogate(reference[i])) {
                if (++i >= reference.Length || !char.IsLowSurrogate(reference[i])) throw new InvalidOperationException("Invalid credential reference");
            } else if (char.IsLowSurrogate(reference[i])) throw new InvalidOperationException("Invalid credential reference");
        }
        if (string.IsNullOrWhiteSpace(reference) || length > schema.GetProperty("maxLength").GetInt32() || reference.Any(char.IsControl))
            throw new InvalidOperationException("Invalid credential reference");
    }
    private async Task<JsonElement> CredentialOperation(object command, bool mutation, string reference, bool deletion, CancellationToken cancellation)
    {
        using var operation = Borrow();
        await operations.WaitAsync(cancellation).ConfigureAwait(false);
        var started = false;
        try {
            Alive();
            if (mutation) { Ready(); reviewed = null; State = HubEditorState.Editing; }
            using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation, lifetime.Token);
            timer.CancelAfter(TimeSpan.FromSeconds(15)); started = true;
            var result = await Rpc(command, timer.Token).ConfigureAwait(false);
            Alive();
            try {
                if (deletion) {
                    HubWire.Members(result, "removed", "persistenceWarning");
                    if (result.GetProperty("removed").ValueKind is not (JsonValueKind.True or JsonValueKind.False)) throw new FormatException();
                    if (result.TryGetProperty("persistenceWarning", out var warning) && warning.ValueKind != JsonValueKind.String) throw new FormatException();
                } else {
                    HubWire.Members(result, "reference", "present", "protection");
                    if (result.GetProperty("reference").GetString() != reference ||
                        result.GetProperty("present").ValueKind is not (JsonValueKind.True or JsonValueKind.False) ||
                        mutation && !result.GetProperty("present").GetBoolean() ||
                        result.GetProperty("protection").GetString() != CredentialDescription!.Value.GetProperty("protection").GetString()) throw new FormatException();
                }
            } catch { throw new HubException(HubFailure.Protocol); }
            return result.Clone();
        } catch (HubException error) {
            if (started && (error.Failure is not (HubFailure.Remote or HubFailure.Busy or HubFailure.InvalidRequest) ||
                error.Remote?.Code is "uncertain" or "unavailable" or "timeout" or "disconnected" or "credentialUnavailable")) {
                reviewed = null; State = HubEditorState.Uncertain;
            }
            throw;
        } catch (OperationCanceledException) {
            if (started) { reviewed = null; State = HubEditorState.Uncertain; }
            throw;
        } finally { operations.Release(); }
    }
}
