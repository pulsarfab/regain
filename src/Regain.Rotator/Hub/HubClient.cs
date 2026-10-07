using System.IO;
using System.Text.Json;

namespace Regain.Hub;

/// A failed client is terminal. Explicitly attach again and acquire new leases;
/// neither connection failure nor caller cancellation replays a command.
public sealed partial class HubClient : IDisposable
{
    private readonly Connection state;
    public HubHello Hello => state.Hello;
    public bool IsConnected => !state.Closed.IsCompleted;
    public Task Closed => state.Closed;
    /// Check interface contracts before acquiring equipment. Negotiation alone
    /// neither dispatches a request nor retires other clients of the host.
    public void RequireCapabilities(params string[] required)
    {
        if (!IsConnected) throw new HubException(HubFailure.Disconnected);
        if (required.Any(capability => !Hello.Capabilities.Contains(capability, StringComparer.Ordinal)))
            throw new HubException(HubFailure.Protocol);
    }
    private HubClient(Connection state) { this.state = state; }
    public Task<JsonElement> RequestAsync(JsonElement command, CancellationToken cancellation = default) => state.Request(command, cancellation);
    public void Dispose() { state.Close(HubFailure.Disconnected); GC.SuppressFinalize(this); }
    ~HubClient() { state.Close(HubFailure.Disconnected); }

    // Tests inject an authenticated duplex stream. Production uses the checked
    // local named pipe below; arbitrary stream injection is not a public API.
    internal static async Task<HubClient> FromStreamAsync(Stream stream, Guid instance, TimeSpan deadline,
        HubClientLimits? limits = null, CancellationToken cancellation = default)
    {
        if (instance == Guid.Empty || deadline <= TimeSpan.Zero || deadline > TimeSpan.FromSeconds(300)) {
            stream.Dispose(); throw new HubException(HubFailure.InvalidRequest);
        }
        limits ??= new HubClientLimits();
        using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        timer.CancelAfter(deadline);
        using var closing = timer.Token.Register(() => { try { stream.Dispose(); } catch { } });
        try {
            var hello = JsonSerializer.SerializeToElement(new { op = "hello" });
            var request = HubWire.Encode(1, hello, HubWire.MaxFrame);
            try { await HubWire.WriteFrame(stream, request, deadline, timer.Token).ConfigureAwait(false); }
            finally { Array.Clear(request, 0, request.Length); }
            var bytes = await HubWire.ReadFrame(stream, HubWire.MaxFrame, deadline, timer.Token).ConfigureAwait(false);
            JsonElement reply;
            try { reply = HubWire.Parse(bytes); } finally { Array.Clear(bytes, 0, bytes.Length); }
            HubWire.Members(reply, "version", "id", "result");
            if (reply.GetProperty("version").GetInt32() != 1 || reply.GetProperty("id").GetUInt64() != 1)
                throw new HubException(HubFailure.Protocol);
            var description = new HubHello(reply.GetProperty("result"), instance);
            cancellation.ThrowIfCancellationRequested();
            if (timer.IsCancellationRequested) throw new HubException(HubFailure.Timeout);
            var state = new Connection(stream, description, limits);
            state.Start();
            return new HubClient(state);
        } catch (Exception) {
            stream.Dispose();
            cancellation.ThrowIfCancellationRequested();
            if (timer.IsCancellationRequested) throw new HubException(HubFailure.Timeout);
            throw new HubException(HubFailure.Protocol);
        }
    }

    private sealed class Pending
    {
        internal ulong Id;
        internal bool Sent, Finished;
        internal readonly bool Mutating;
        internal readonly TaskCompletionSource<JsonElement> Reply = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal readonly CancellationTokenSource Done = new();
        internal readonly CancellationToken Caller;
        internal readonly TimeSpan Timeout;
        internal Pending(bool mutating, CancellationToken caller, TimeSpan timeout) { Mutating = mutating; Caller = caller; Timeout = timeout; }
    }
    // Pump tasks retain only Connection, so an abandoned public client can be
    // finalized and close its stream instead of keeping its leases alive forever.
    private sealed class Connection
    {
        private static readonly string[] knownOperations = ["cameraTiming", "cameraControl", "describeConfig", "getConfig", "validateConfig", "applyConfig",
            "listDevices", "sourceStatus", "outputStatus", "hostStatus", "inspectSource", "updateSimulation", "createCredential",
            "credentialStatus", "deleteCredential", "connect", "disconnect", "changeConnection", "get", "put"];
        private readonly object gate = new();
        private readonly Stream stream;
        private readonly HubClientLimits limits;
        private readonly HashSet<Pending> admitted = [];
        private readonly Dictionary<ulong, Pending> sent = [];
        private readonly SemaphoreSlim writer = new(1, 1);
        private readonly CancellationTokenSource stop = new();
        private readonly TaskCompletionSource<bool> closed = new(TaskCreationOptions.RunContinuationsAsynchronously);
        private ulong next = 2;
        internal HubHello Hello { get; }
        internal Task Closed => closed.Task;
        internal Connection(Stream stream, HubHello hello, HubClientLimits limits) { this.stream = stream; Hello = hello; this.limits = limits; }
        internal void Start() { _ = ReadLoop(); }
        internal Task<JsonElement> Request(JsonElement command, CancellationToken caller, TimeSpan? cameraDeadline = null)
        {
            caller.ThrowIfCancellationRequested();
            string operation;
            try {
                if (command.ValueKind != JsonValueKind.Object) throw new HubException(HubFailure.InvalidRequest);
                operation = command.GetProperty("op").GetString() ?? "";
                if (!knownOperations.Contains(operation, StringComparer.Ordinal) || !Hello.Operations.Contains(operation, StringComparer.Ordinal))
                    throw new HubException(HubFailure.InvalidRequest);
                long budget = Hello.MaxFrameBytes - 80;
                HubWire.CheckTokens(command, ref budget);
                HubWire.Unique(command);
            } catch (Exception) { throw new HubException(HubFailure.InvalidRequest); }
            var deadline = cameraDeadline.HasValue ? cameraDeadline.Value + limits.FrameTimeout + limits.FrameTimeout : limits.RequestTimeout;
            if (deadline < limits.RequestTimeout) deadline = limits.RequestTimeout;
            var p = new Pending(operation is "cameraControl" or "put" or "connect" or "disconnect" or "changeConnection" or
                "applyConfig" or "createCredential" or "deleteCredential" or "updateSimulation", caller, deadline);
            lock (gate) {
                if (closed.Task.IsCompleted) throw new HubException(HubFailure.Disconnected);
                if (admitted.Count >= Hello.MaxInFlight) throw new HubException(HubFailure.Busy);
                admitted.Add(p);
            }
            // Clone only after admission. Background tasks cannot use an element
            // whose caller-owned JsonDocument has been disposed.
            JsonElement owned;
            try { owned = command.Clone(); }
            catch { Finish(p, null, new HubException(HubFailure.InvalidRequest)); return p.Reply.Task; }
            _ = Send(p, owned);
            _ = Deadline(p);
            return p.Reply.Task;
        }
        private async Task Deadline(Pending p)
        {
            try { await Task.Delay(p.Timeout, p.Done.Token).ConfigureAwait(false); Close(HubFailure.Timeout); }
            catch (OperationCanceledException) { }
        }
        private async Task Send(Pending p, JsonElement command)
        {
            byte[]? bytes = null;
            var acquired = false;
            using var cancellation = p.Caller.Register(() => CancelCaller(p));
            using var queued = CancellationTokenSource.CreateLinkedTokenSource(stop.Token, p.Caller, p.Done.Token);
            try {
                await writer.WaitAsync(queued.Token).ConfigureAwait(false); acquired = true;
                lock (gate) {
                    if (p.Finished) return;
                    if (p.Caller.IsCancellationRequested) { FinishLocked(p, null, null); return; }
                    if (next == ulong.MaxValue) throw new HubException(HubFailure.Protocol);
                    p.Id = next++;
                }
                bytes = HubWire.Encode(p.Id, command, Hello.MaxFrameBytes);
                command = default;
                lock (gate) {
                    if (p.Finished) return;
                    if (p.Caller.IsCancellationRequested) { FinishLocked(p, null, null); return; }
                    p.Sent = true;
                    sent.Add(p.Id, p);
                }
                await HubWire.WriteFrame(stream, bytes, limits.FrameTimeout, stop.Token).ConfigureAwait(false);
            } catch (OperationCanceledException) when (!p.Sent) {
                Finish(p, null, p.Caller.IsCancellationRequested ? null : new HubException(HubFailure.Disconnected));
            } catch (HubException e) when (!p.Sent && e.Failure == HubFailure.InvalidRequest) {
                Finish(p, null, e);
            } catch (Exception e) {
                Close(e is HubException hub ? hub.Failure : HubFailure.Disconnected);
            } finally {
                if (bytes is not null) Array.Clear(bytes, 0, bytes.Length);
                if (acquired) writer.Release();
            }
            // Keep caller cancellation active after dispatch, while the slot and
            // request deadline remain until a reply or terminal connection loss.
            if (p.Sent) {
                try { await Task.Delay(Timeout.Infinite, p.Done.Token).ConfigureAwait(false); }
                catch (OperationCanceledException) { }
            }
        }
        private async Task ReadLoop()
        {
            try {
                while (true) {
                    var bytes = await HubWire.ReadFrame(stream, Hello.MaxFrameBytes, limits.FrameTimeout, stop.Token).ConfigureAwait(false);
                    JsonElement reply;
                    try { reply = HubWire.Parse(bytes); } finally { Array.Clear(bytes, 0, bytes.Length); }
                    HubWire.Members(reply, "version", "id", "result", "error");
                    if (reply.GetProperty("version").GetInt32() != 1) throw new HubException(HubFailure.Protocol);
                    var id = reply.GetProperty("id").GetUInt64();
                    var hasResult = reply.TryGetProperty("result", out var value);
                    var hasError = reply.TryGetProperty("error", out var error);
                    if (hasResult == hasError) throw new HubException(HubFailure.Protocol);
                    HubException? failure = hasError ? new HubException(HubFailure.Remote, new HubRemoteError(error)) : null;
                    lock (gate) {
                        if (!sent.TryGetValue(id, out var p)) throw new HubException(HubFailure.Protocol);
                        FinishLocked(p, hasResult ? value : null, failure);
                    }
                }
            } catch (Exception e) {
                Close(e is HubException hub ? hub.Failure : e is IOException or ObjectDisposedException or OperationCanceledException ? HubFailure.Disconnected : HubFailure.Protocol);
            }
        }
        private void Finish(Pending p, JsonElement? value, HubException? error) { lock (gate) FinishLocked(p, value, error); }
        private void CancelCaller(Pending p)
        {
            lock (gate) {
                p.Reply.TrySetCanceled();
                if (!p.Sent) FinishLocked(p, null, null);
            }
        }
        private void FinishLocked(Pending p, JsonElement? value, HubException? error)
        {
            if (p.Finished) return;
            p.Finished = true;
            admitted.Remove(p);
            if (p.Sent) sent.Remove(p.Id);
            p.Done.Cancel();
            if (error is not null) p.Reply.TrySetException(error);
            else if (value.HasValue) p.Reply.TrySetResult(value.Value.Clone());
            else p.Reply.TrySetCanceled();
        }
        internal void Close(HubFailure reason)
        {
            lock (gate) {
                if (closed.Task.IsCompleted) return;
                closed.TrySetResult(true);
                foreach (var p in admitted.ToArray())
                    FinishLocked(p, null, new HubException(p.Sent && p.Mutating ? HubFailure.Uncertain : reason));
            }
            stop.Cancel();
            try { stream.Dispose(); } catch { }
        }
    }
}
