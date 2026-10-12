namespace Regain.NINA;

internal sealed record UsbPeripheralRecoveryOptions
{
    public bool Enabled { get; init; } = true;
    public int DisconnectWindowSeconds { get; init; } = 15;
    public void Validate()
    {
        if (DisconnectWindowSeconds is < 1 or > 60)
            throw new ArgumentOutOfRangeException(nameof(DisconnectWindowSeconds), "Use a peripheral disconnect window from 1 to 60 seconds.");
    }
}

internal sealed record PeripheralState(bool Connected, string DeviceId, string? ImagingState = null);

internal interface IUsbPeripheral : IDisposable
{
    string Kind { get; }
    bool RequiresImagingState => false;
    PeripheralState Snapshot();
    event Action? StateChanged;
    Task<bool> ReconnectAsync(CancellationToken token);
}

/// <summary>
/// Correlates NINA disconnect reports with this camera's USB reset. It never
/// changes device selection, moves equipment, restores filters, or starts guiding.
/// The supervisor holds replacement exposures until WaitAsync succeeds.
/// </summary>
internal sealed class UsbPeripheralRecovery : IDisposable
{
    private sealed class Episode(ulong id, long started, CancellationTokenSource cancellation,
        Dictionary<IUsbPeripheral, PeripheralState> before)
    {
        internal ulong Id { get; } = id;
        internal long Started { get; } = started;
        internal CancellationTokenSource Cancellation { get; } = cancellation;
        internal Dictionary<IUsbPeripheral, PeripheralState> Before { get; } = before;
        internal HashSet<IUsbPeripheral> Dropped { get; } = [];
        internal TaskCompletionSource Returned { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal Task Work { get; set; } = Task.CompletedTask;
        internal void Cancel() { try { Cancellation.Cancel(); } catch (ObjectDisposedException) { } }
    }
    private readonly object sync = new();
    private readonly IReadOnlyList<IUsbPeripheral> peripherals;
    private readonly UsbPeripheralRecoveryOptions options;
    private readonly Action<string> log;
    private readonly TimeProvider clock;
    private readonly Dictionary<IUsbPeripheral, PeripheralState> observed = [];
    private readonly Dictionary<IUsbPeripheral, PeripheralState> lastConnected = [];
    private readonly Dictionary<IUsbPeripheral, PeripheralState> beforeDisconnect = [];
    private readonly Dictionary<IUsbPeripheral, long> disconnectedAtUnixUs = [];
    private readonly Dictionary<IUsbPeripheral, Action> handlers = [];
    private readonly CancellationTokenSource lifetime = new();
    private TaskCompletionSource anchorChanged = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private Episode? episode;
    private bool disposed;
    internal bool Enabled => options.Enabled;

    internal UsbPeripheralRecovery(IReadOnlyList<IUsbPeripheral> peripherals,
        UsbPeripheralRecoveryOptions options, Action<string> log, TimeProvider? clock = null)
    {
        options.Validate();
        this.peripherals = peripherals; this.options = options; this.log = log;
        this.clock = clock ?? TimeProvider.System;
        foreach (var peripheral in peripherals) {
            Action handler = () => Observe(peripheral);
            handlers.Add(peripheral, handler);
            peripheral.StateChanged += handler;
            Observe(peripheral);
        }
    }

    internal void ResetStarting(ulong? id, long? timestampUnixUs = null)
    {
        // Capture the latest cached positions immediately before cycling. If
        // USB already dropped before stderr delivery, retain the last connected
        // snapshot supplied by the device's regular status notifications.
        foreach (var peripheral in peripherals) {
            try {
                var latest = peripheral.Snapshot();
                if (latest.Connected) lock (sync) { observed[peripheral] = latest; lastConnected[peripheral] = latest; }
            } catch { /* An unavailable snapshot is checked if the device drops. */ }
        }
        Episode current;
        lock (sync) {
            if (disposed || !options.Enabled || id is null or 0) return;
            if (episode is { Work.IsCompleted: false }) {
                // A second native reset must not overlap an unresolved NINA
                // connection operation. Fail this capture rather than replay it.
                episode.Cancel();
                return;
            }
            episode?.Cancellation.Dispose();
            var cancellation = CancellationTokenSource.CreateLinkedTokenSource(lifetime.Token);
            cancellation.CancelAfter(TimeSpan.FromSeconds(210));
            long nowUnixUs = UnixUs();
            long resetUnixUs = timestampUnixUs is > 0 && timestampUnixUs <= nowUnixUs ? timestampUnixUs.Value : nowUnixUs;
            long windowEnd = resetUnixUs + options.DisconnectWindowSeconds * 1_000_000L;
            var before = observed.Where(pair => pair.Value.Connected && !string.IsNullOrWhiteSpace(pair.Value.DeviceId))
                .ToDictionary(pair => pair.Key, pair => pair.Value);
            // A disconnect notification can beat asynchronous stderr delivery.
            // Retain the last connected state, but include it only when its
            // recorded disconnect is after this exact supervisor reset anchor.
            foreach (var (peripheral, when) in disconnectedAtUnixUs)
                if (when >= resetUnixUs && when <= windowEnd && beforeDisconnect.TryGetValue(peripheral, out var state)
                    && !string.IsNullOrWhiteSpace(state.DeviceId)) before[peripheral] = state;
            double lagSeconds = Math.Clamp((nowUnixUs - resetUnixUs) / 1_000_000.0, 0, 210);
            long started = clock.GetTimestamp() - (long)(lagSeconds * clock.TimestampFrequency);
            current = new(id.Value, started, cancellation, before);
            foreach (var (peripheral, when) in disconnectedAtUnixUs)
                if (before.ContainsKey(peripheral) && when >= resetUnixUs && when <= windowEnd) current.Dropped.Add(peripheral);
            episode = current;
            anchorChanged.TrySetResult();
            anchorChanged = new(TaskCreationOptions.RunContinuationsAsynchronously);
            current.Work = RunAsync(current);
            // Faults are retained for the capture barrier even if the camera
            // never returns and its capture exits before asking us to wait.
            _ = current.Work.ContinueWith(task => { _ = task.Exception; },
                CancellationToken.None, TaskContinuationOptions.OnlyOnFaulted | TaskContinuationOptions.ExecuteSynchronously,
                TaskScheduler.Default);
        }
        Write($"Camera USB recovery: observing previously connected NINA peripherals for {options.DisconnectWindowSeconds} seconds.");
    }

    internal void CameraReturned(ulong? id)
    {
        lock (sync) if (episode is { } current && current.Id == id) {
            current.Cancellation.CancelAfter(TimeSpan.FromSeconds(105));
            current.Returned.TrySetResult();
        }
    }

    internal void Failed()
    {
        lock (sync) episode?.Cancel();
    }

    internal async Task WaitAsync(ulong id, CancellationToken token)
    {
        Episode current;
        // Stderr delivery can lag a status reply. Await this exact reset anchor;
        // a previous successful reset must never authorize the next exposure.
        using var anchorDeadline = CancellationTokenSource.CreateLinkedTokenSource(token, lifetime.Token);
        anchorDeadline.CancelAfter(TimeSpan.FromSeconds(10));
        while (true) {
            Task changed;
            lock (sync) {
                if (episode is { } active && active.Id == id) { current = active; break; }
                changed = anchorChanged.Task;
            }
            try { await changed.WaitAsync(anchorDeadline.Token).ConfigureAwait(false); }
            catch (OperationCanceledException error) when (!token.IsCancellationRequested && !lifetime.IsCancellationRequested) {
                throw new IOException("No matching live USB recovery was observed; replacement exposure is blocked.", error);
            }
        }
        try { await current.Work.WaitAsync(token).ConfigureAwait(false); }
        catch { current.Cancel(); throw; }
    }

    private void Observe(IUsbPeripheral peripheral)
    {
        PeripheralState state;
        try { state = peripheral.Snapshot(); }
        catch (Exception error) {
            Write($"Unable to read {peripheral.Kind} connection state: {error.Message}");
            return;
        }
        lock (sync) {
            if (disposed) return;
            if (state.Connected) {
                lastConnected[peripheral] = state;
            } else if (observed.TryGetValue(peripheral, out var previous) && previous.Connected) {
                disconnectedAtUnixUs[peripheral] = UnixUs();
                beforeDisconnect[peripheral] = lastConnected.GetValueOrDefault(peripheral, previous);
            }
            if (episode is { } current && current.Before.ContainsKey(peripheral) && !state.Connected
                && clock.GetElapsedTime(current.Started).TotalSeconds <= options.DisconnectWindowSeconds)
                current.Dropped.Add(peripheral);
            observed[peripheral] = state;
        }
    }

    private async Task RunAsync(Episode current)
    {
        var token = current.Cancellation.Token;
        // Yield so the episode is fully assigned before a device notification
        // or a very fast simulated return can reach it.
        await Task.Yield();
        while (clock.GetElapsedTime(current.Started).TotalSeconds < options.DisconnectWindowSeconds) {
            token.ThrowIfCancellationRequested();
            foreach (var peripheral in peripherals) Observe(peripheral);
            var remaining = TimeSpan.FromSeconds(options.DisconnectWindowSeconds) - clock.GetElapsedTime(current.Started);
            if (remaining > TimeSpan.Zero)
                await Task.Delay(remaining < TimeSpan.FromMilliseconds(250) ? remaining : TimeSpan.FromMilliseconds(250), clock, token).ConfigureAwait(false);
        }
        await current.Returned.Task.WaitAsync(token).ConfigureAwait(false);
        IUsbPeripheral[] dropped;
        lock (sync) dropped = current.Dropped.ToArray();
        foreach (var peripheral in dropped) {
            token.ThrowIfCancellationRequested();
            var before = current.Before[peripheral];
            if (peripheral.RequiresImagingState && before.ImagingState is null)
                throw new IOException($"{peripheral.Kind} optical state was unavailable before USB recovery; automatic reconnection and replacement exposure are blocked.");
            var state = peripheral.Snapshot();
            RequireSameDevice(peripheral, before, state);
            if (!state.Connected) {
                Write($"Reconnecting {peripheral.Kind}: it disconnected within the camera USB recovery window.");
                bool connected = false;
                for (int attempt = 1; attempt <= 3; attempt++) {
                    token.ThrowIfCancellationRequested();
                    RequireSameDevice(peripheral, before, peripheral.Snapshot());
                    using var attemptDeadline = CancellationTokenSource.CreateLinkedTokenSource(token);
                    attemptDeadline.CancelAfter(TimeSpan.FromSeconds(20));
                    try {
                        // NINA does not expose cancellation for these commands.
                        // A timed-out command is never overlapped by another try.
                        connected = await peripheral.ReconnectAsync(attemptDeadline.Token)
                            .WaitAsync(attemptDeadline.Token).ConfigureAwait(false);
                    }
                    catch (OperationCanceledException) {
                        throw new IOException($"{peripheral.Kind} reconnect timed out or was cancelled; no further connection attempt or replacement exposure is allowed.");
                    }
                    catch (Exception error) {
                        Write($"{peripheral.Kind} reconnect attempt {attempt}/3 failed: {error.Message}");
                    }
                    state = peripheral.Snapshot();
                    RequireSameDevice(peripheral, before, state);
                    if (connected && state.Connected) break;
                    if (attempt < 3) await Task.Delay(TimeSpan.FromSeconds(2), clock, token).ConfigureAwait(false);
                }
                if (!connected || !state.Connected)
                    throw new IOException($"{peripheral.Kind} did not reconnect; replacement exposure is blocked.");
            }
            if (before.ImagingState is { } expected && state.ImagingState != expected)
                throw new IOException($"{peripheral.Kind} reconnected with a changed or unavailable optical state ({expected} -> {state.ImagingState ?? "unknown"}); check the equipment before capturing again.");
            Write($"{peripheral.Kind} connection restored; captured device identity and reported optical state match.");
        }
        Write(dropped.Length == 0 ? "No previously connected NINA peripheral reported a disconnect in the camera USB recovery window."
            : $"Camera USB recovery: {dropped.Length} NINA peripheral connection(s) restored. Guiding is not restarted by Regain.");
    }

    private static void RequireSameDevice(IUsbPeripheral peripheral, PeripheralState before, PeripheralState state)
    {
        if (string.IsNullOrWhiteSpace(state.DeviceId) || state.DeviceId != before.DeviceId)
            throw new IOException($"{peripheral.Kind} selection changed or cannot be verified; automatic reconnect and replacement exposure are blocked.");
    }
    private long UnixUs() => (clock.GetUtcNow().Ticks - DateTimeOffset.UnixEpoch.Ticks) / 10;
    private void Write(string message) { try { log(message); } catch { /* A failed log sink cannot change recovery. */ } }
    public void Dispose()
    {
        lock (sync) {
            if (disposed) return;
            disposed = true;
            lifetime.Cancel();
            episode?.Cancel();
            episode?.Cancellation.Dispose();
        }
        foreach (var (peripheral, handler) in handlers) {
            peripheral.StateChanged -= handler;
            peripheral.Dispose();
        }
        lifetime.Dispose();
    }
}
