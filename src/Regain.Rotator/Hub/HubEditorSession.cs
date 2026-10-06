using System.Text.Json;

namespace Regain.Hub;

public enum HubEditorState { Unloaded, Loading, Editing, Reviewing, Reviewed, Applying, Uncertain, Blocked, Disposed }

/// A private setup client has no output leases. Review never connects sources;
/// Apply is a single revision-checked request to the shared host supervisor.
public sealed partial class HubEditorSession : IDisposable
{
    private readonly Func<JsonElement, CancellationToken, Task<JsonElement>> request;
    private readonly Action close;
    private readonly SemaphoreSlim operations = new(1, 1);
    private readonly CancellationTokenSource lifetime = new();
    private readonly object lifecycle = new();
    private int activeOperations;
    private HubEditorState state;
    private JsonElement? reviewed;
    private long reviewedVersion;
    private volatile bool disposed;
    public Guid InstanceId { get; }
    public HubConfigurationDraft? Draft { get; private set; }
    public HubEditorState State {
        get { lock (lifecycle) return state; }
        private set { lock (lifecycle) { if (!disposed) state = value; } }
    }
    public JsonElement? HostStatus { get; private set; }
    public JsonElement? Description { get; private set; }
    public JsonElement? SavedConfiguration { get; private set; }
    public JsonElement? LastApply { get; private set; }
    public JsonElement Errors { get; private set; } = JsonSerializer.SerializeToElement(Array.Empty<object>());
    internal HubEditorSession(Guid instance, Func<JsonElement, CancellationToken, Task<JsonElement>> request, Action close)
    { InstanceId = instance; this.request = request; this.close = close; }
    public static async Task<HubEditorSession> AttachAsync(string executable, string configPath, Guid expectedInstance,
        CancellationToken cancellation = default)
    {
        var attachment = await HubAttachment.AttachAsync(executable, configPath, cancellation: cancellation, expectedInstance: expectedInstance).ConfigureAwait(false);
        var client = await HubClient.ConnectAsync(attachment, cancellation: cancellation).ConfigureAwait(false);
        return new(attachment.InstanceId, client.RequestAsync, client.Dispose);
    }
    private Task<JsonElement> Rpc(object command, CancellationToken token) => request(JsonSerializer.SerializeToElement(command), token);
    public async Task ReloadAsync(CancellationToken cancellation = default)
    {
        using var operation = Borrow();
        await operations.WaitAsync(cancellation).ConfigureAwait(false);
        try {
            Alive(); using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation, lifetime.Token);
            timer.CancelAfter(TimeSpan.FromSeconds(15));
            reviewed = null; LastSourceObservation = null; State = HubEditorState.Loading;
            var description = await Rpc(new { op = "describeConfig" }, timer.Token).ConfigureAwait(false);
            var saved = await Rpc(new { op = "getConfig" }, timer.Token).ConfigureAwait(false);
            var status = await Rpc(new { op = "hostStatus" }, timer.Token).ConfigureAwait(false);
            Alive();
            if (saved.GetProperty("instanceId").GetGuid() != InstanceId ||
                status.GetProperty("configurationRevision").GetGuid() != saved.GetProperty("revision").GetGuid())
                throw new InvalidOperationException("The saved configuration changed during reload; reload again");
            var draft = new HubConfigurationDraft(description, saved);
            Draft = draft; SavedConfiguration = saved.Clone(); Description = description.Clone(); HostStatus = status.Clone(); Errors = JsonSerializer.SerializeToElement(Array.Empty<object>());
            State = status.GetProperty("phase").GetString() == "ready" ? HubEditorState.Editing : HubEditorState.Blocked;
        } catch { if (!disposed) State = HubEditorState.Uncertain; throw; }
        finally { operations.Release(); }
    }
    public void Changed()
    {
        Alive(); if (State is not (HubEditorState.Editing or HubEditorState.Reviewed)) throw new InvalidOperationException("The editor is not ready for changes");
        reviewed = null; State = HubEditorState.Editing;
    }
    public async Task<bool> ReviewAsync(CancellationToken cancellation = default)
    {
        using var operation = Borrow();
        await operations.WaitAsync(cancellation).ConfigureAwait(false);
        bool started = false;
        try {
            Ready(); State = HubEditorState.Reviewing; reviewed = null; started = true;
            var version = Draft!.Version; var candidate = Draft.Candidate;
            using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation, lifetime.Token);
            timer.CancelAfter(TimeSpan.FromSeconds(15));
            var result = await Rpc(new { op = "validateConfig", candidate }, timer.Token).ConfigureAwait(false);
            Alive();
            if (!result.TryGetProperty("errors", out var errors) || errors.ValueKind != JsonValueKind.Array ||
                !result.TryGetProperty("valid", out var validity) || validity.ValueKind is not (JsonValueKind.True or JsonValueKind.False) ||
                errors.EnumerateArray().Any(e => e.ValueKind != JsonValueKind.Object || !e.TryGetProperty("path", out var path) || path.ValueKind != JsonValueKind.String ||
                    !e.TryGetProperty("message", out var message) || message.ValueKind != JsonValueKind.String))
                throw new HubException(HubFailure.Protocol);
            Errors = errors.Clone();
            var valid = result.GetProperty("valid").GetBoolean();
            if (valid && Errors.GetArrayLength() != 0) throw new HubException(HubFailure.Protocol);
            if (Draft.Version != version || Draft.Candidate.GetRawText() != candidate.GetRawText())
                throw new InvalidOperationException("The draft changed during review; review it again");
            if (valid) { reviewed = candidate; reviewedVersion = version; State = HubEditorState.Reviewed; }
            else State = HubEditorState.Editing;
            return valid;
        } catch (HubException error) {
            if (started && !disposed) State = error.Failure is HubFailure.Remote or HubFailure.Busy or HubFailure.InvalidRequest ? HubEditorState.Editing : HubEditorState.Uncertain;
            reviewed = null; throw;
        } catch (OperationCanceledException) {
            if (started && !disposed) State = HubEditorState.Uncertain;
            reviewed = null; throw;
        } catch { if (started && !disposed) State = HubEditorState.Editing; reviewed = null; throw; }
        finally { operations.Release(); }
    }
    public async Task<JsonElement> ApplyAsync(CancellationToken cancellation = default)
    {
        using var operation = Borrow();
        await operations.WaitAsync(cancellation).ConfigureAwait(false);
        bool started = false;
        try {
            Alive();
            if (State != HubEditorState.Reviewed || Draft is null || !reviewed.HasValue || Draft.Version != reviewedVersion ||
                Draft.Candidate.GetRawText() != reviewed.Value.GetRawText())
                throw new InvalidOperationException("Review the current draft before applying it");
            var candidate = reviewed.Value; var revision = Draft.Revision;
            reviewed = null; State = HubEditorState.Applying; started = true;
            using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation, lifetime.Token);
            timer.CancelAfter(TimeSpan.FromSeconds(35));
            var outcome = await Rpc(new { op = "applyConfig", expectedRevision = revision, candidate }, timer.Token).ConfigureAwait(false);
            Alive(); LastApply = outcome.Clone();
            if (!outcome.GetProperty("applied").GetBoolean() || outcome.GetProperty("configurationRevision").GetGuid() == Guid.Empty ||
                outcome.GetProperty("configurationRevision").GetGuid() == revision || outcome.GetProperty("ready").ValueKind is not (JsonValueKind.True or JsonValueKind.False))
                throw new HubException(HubFailure.Protocol);
            // A successful response still needs the saved configuration/status.
            // Failure to reconcile cannot authorize another Apply of the draft.
            State = HubEditorState.Uncertain;
            return outcome.Clone();
        } catch (HubException error) when (started && error.Failure is HubFailure.Remote or HubFailure.InvalidRequest or HubFailure.Busy) {
            if (!disposed) State = error.Remote?.Code is "uncertain" or "revisionConflict" or "unavailable" or "timeout" or "disconnected"
                ? HubEditorState.Uncertain : HubEditorState.Editing;
            Errors = error.Remote?.Fields ?? JsonSerializer.SerializeToElement(Array.Empty<object>());
            throw;
        } catch { if (started && !disposed) State = HubEditorState.Uncertain; throw; }
        finally { operations.Release(); }
    }
    public async Task<JsonElement> SourceStatusAsync(Guid source, CancellationToken cancellation = default)
    {
        using var operation = Borrow();
        await operations.WaitAsync(cancellation).ConfigureAwait(false);
        try {
            Alive(); SavedSource(source); LastSourceObservation = null;
            using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation, lifetime.Token);
            timer.CancelAfter(TimeSpan.FromSeconds(3));
            var result = await Rpc(new { op = "sourceStatus", source }, timer.Token).ConfigureAwait(false);
            Alive();
            try {
                HubWire.Members(result, "source", "revision", "generation", "sequence", "transportConnected", "writeUncertain", "connectionInfo",
                    "simulated", "simulation", "leaseCount", "values", "sampleErrors", "sampleAgesSeconds", "sampleStartedSeconds", "sampleSequences",
                    "completedPasses", "sampledAtSeconds", "error");
                if (result.GetProperty("source").GetGuid() != source || result.GetProperty("revision").GetGuid() != Draft!.Revision)
                    throw new FormatException();
            } catch { throw new HubException(HubFailure.Protocol); }
            LastSourceObservation = Observation("cachedSourceHealth", result);
            return result;
        } catch (HubException error) when (error.Failure is not (HubFailure.Remote or HubFailure.Busy or HubFailure.InvalidRequest)) {
            reviewed = null; State = HubEditorState.Uncertain; throw;
        } catch (OperationCanceledException) { reviewed = null; State = HubEditorState.Uncertain; throw; }
        finally { operations.Release(); }
    }
    private void Ready()
    {
        Alive(); if (Draft is null || State is not (HubEditorState.Editing or HubEditorState.Reviewed)) throw new InvalidOperationException("Reload the saved configuration and host status first");
    }
    private void Alive() { if (disposed) throw new ObjectDisposedException(nameof(HubEditorSession)); }
    private IDisposable Borrow()
    {
        lock (lifecycle) { Alive(); activeOperations++; }
        return new SessionUse(this);
    }
    private void Release()
    {
        lock (lifecycle) { if (--activeOperations == 0 && disposed) lifetime.Dispose(); }
    }
    private sealed class SessionUse(HubEditorSession owner) : IDisposable
    {
        private HubEditorSession? session = owner;
        public void Dispose() => Interlocked.Exchange(ref session, null)?.Release();
    }
    public void Dispose()
    {
        lock (lifecycle) {
            if (disposed) return;
            disposed = true; state = HubEditorState.Disposed;
            // Keep the token source alive through cancellation callbacks and
            // any concurrent operation; close only this private setup client.
            activeOperations++;
        }
        try { lifetime.Cancel(); }
        finally { try { close(); } finally { Release(); } }
    }
}
