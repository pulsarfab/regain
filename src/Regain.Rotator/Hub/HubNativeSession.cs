using System.Text.Json;

namespace Regain.Hub;

/// One native device's private client. The shared Rust host owns sources and
/// policy. There is no HTTP listener or ASCOM-output dependency here.
public sealed class HubNativeSession(string executable, string? workers = null) : IDisposable
{
    private readonly object gate = new();
    private HubClient? client;
    private CancellationTokenSource? pending;
    private bool leased, disposed;
    private Guid epoch = Guid.NewGuid();
    public Guid Epoch { get { lock (gate) return epoch; } }
    public bool IsAttached { get { lock (gate) return client?.IsConnected == true; } }
    public void RequireCapabilities(params string[] required)
    {
        lock (gate) {
            if (client is null) throw new HubException(HubFailure.Disconnected);
            client.RequireCapabilities(required);
        }
    }
    public bool Connected { get { lock (gate) return leased && client?.IsConnected == true; } }
    public Task<JsonElement> ConnectAsync(HubSelection selection, CancellationToken cancellation) => OpenAsync(selection, true, cancellation);
    /// Attach to a verified output without acquiring equipment. Native ASCOM
    /// uses the host's changeConnection operation and completion state directly.
    public Task<JsonElement> AttachAsync(HubSelection selection, CancellationToken cancellation) => OpenAsync(selection, false, cancellation);
    private async Task<JsonElement> OpenAsync(HubSelection selection, bool acquire, CancellationToken cancellation)
    {
        selection = selection.Copy();
        selection.Validate();
        CancellationTokenSource operation;
        Guid token;
        lock (gate) {
            if (disposed) throw new ObjectDisposedException(nameof(HubNativeSession));
            if (pending is not null || client is not null) throw new InvalidOperationException("This native hub output is already attached or attaching");
            pending = operation = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
            operation.CancelAfter(TimeSpan.FromSeconds(45)); token = epoch = Guid.NewGuid();
        }
        HubClient? opened = null;
        try {
            var attachment = await HubAttachment.AttachAsync(executable, selection.ConfigPath, workers, operation.Token, selection.InstanceId).ConfigureAwait(false);
            if (attachment.InstanceId != selection.InstanceId) throw new InvalidOperationException("The configuration now belongs to a different hub; select its output explicitly");
            opened = await HubClient.ConnectAsync(attachment, cancellation: operation.Token).ConfigureAwait(false);
            var catalog = await opened.RequestAsync(JsonSerializer.SerializeToElement(new { op = "listDevices" }), operation.Token).ConfigureAwait(false);
            var matches = catalog.EnumerateArray().Where(d => d.GetProperty("id").GetGuid() == selection.OutputId &&
                d.GetProperty("deviceType").GetString() == selection.DeviceType).ToArray();
            if (matches.Length != 1) throw new InvalidOperationException("The saved output is missing or has a different class; select it explicitly");
            if (selection.DeviceType == "focuser") opened.RequireCapabilities("focuserOutputs", "scalarDeviceState", "asyncOutputConnection");
            if (selection.DeviceType == "filterwheel") opened.RequireCapabilities("filterWheelOutputs", "scalarDeviceState", "asyncOutputConnection");
            if (selection.DeviceType == "rotator") {
                opened.RequireCapabilities("rotatorOutputs", "scalarDeviceState", "asyncOutputConnection");
                if (acquire) opened.RequireCapabilities("rotatorMotionReceipt");
            }
            lock (gate) {
                if (token != epoch || operation.IsCancellationRequested || disposed) throw new OperationCanceledException(operation.Token);
                client = opened;
            }
            if (acquire) await opened.RequestAsync(JsonSerializer.SerializeToElement(new { op = "changeConnection", output = selection.OutputId,
                connected = true, asynchronous = false }), operation.Token).ConfigureAwait(false);
            lock (gate) {
                if (token != epoch || operation.IsCancellationRequested || disposed) throw new OperationCanceledException(operation.Token);
                leased = acquire;
            }
            return matches[0].Clone();
        } catch {
            opened?.Dispose();
            lock (gate) { if (token == epoch) { client = null; leased = false; epoch = Guid.NewGuid(); } }
            throw;
        } finally {
            lock (gate) { if (ReferenceEquals(pending, operation)) pending = null; }
            operation.Dispose();
        }
    }
    public async Task<JsonElement> RequestAsync(Guid expectedEpoch, JsonElement command,
        TimeSpan? deadline = null, CancellationToken cancellation = default)
    {
        HubClient current;
        lock (gate) {
            if (expectedEpoch != epoch || client?.IsConnected != true) throw new InvalidOperationException("This native hub output is disconnected or belongs to a retired session");
            current = client;
        }
        using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        timer.CancelAfter(deadline ?? TimeSpan.FromSeconds(3));
        try {
            var value = await current.RequestAsync(command, timer.Token).ConfigureAwait(false);
            lock (gate) {
                if (expectedEpoch != epoch || !ReferenceEquals(current, client) || !current.IsConnected)
                    throw new InvalidOperationException("This native hub response belongs to a retired session");
            }
            return value;
        } catch (HubException error) when (error.Failure is HubFailure.Remote or HubFailure.Busy or HubFailure.InvalidRequest) {
            // A failed sensor is not a dead host. Preserve unrelated readings
            // and let the shared engine retain freshness/uncertainty semantics.
            // Local admission/preflight rejection did not retire the transport.
            throw;
        } catch {
            Retire(expectedEpoch); throw;
        }
    }
    private void Retire(Guid expected)
    {
        HubClient? closing;
        lock (gate) {
            if (epoch != expected) return;
            closing = client; client = null; leased = false; epoch = Guid.NewGuid();
        }
        closing?.Dispose();
    }
    public void Disconnect()
    {
        HubClient? closing;
        CancellationTokenSource? cancelling;
        lock (gate) {
            epoch = Guid.NewGuid(); leased = false; closing = client; client = null;
            cancelling = pending;
        }
        // Cancellation callbacks run synchronously: never invoke them under the
        // session gate. Connect may already have finished and disposed its CTS.
        try { cancelling?.Cancel(); } catch (ObjectDisposedException) { }
        closing?.Dispose();
    }
    public void Dispose() { lock (gate) disposed = true; Disconnect(); }
}
