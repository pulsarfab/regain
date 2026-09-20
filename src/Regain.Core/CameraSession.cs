using System.Diagnostics;
using System.Text.Json;

namespace Regain.Core;

/// <summary>Survives host death. One transaction includes expose, transfer, restore and re-expose.</summary>
public sealed class CameraSession : IDisposable
{
    private readonly Func<HostClient> factory;
    private readonly Func<HostClient>? sdkFallbackFactory;
    private bool usingFallback;
    private bool supervised;
    public bool UsingSdkFallback => usingFallback;
    private readonly SemaphoreSlim operation = new(1);
    private readonly object sync = new();
    private readonly Dictionary<int, long> desired = new();
    private readonly Dictionary<int, long> observed = new();
    private readonly Dictionary<int, long> applied = new();
    private HostClient? host;
    private string? serial;
    private bool hasConnected;
    private bool disposed;
    private readonly CancellationTokenSource shutdown = new();
    private Task controlRecovery = Task.CompletedTask;
    private bool requiresReconnect, requiresCoolingSettle;
    private double? recoveryTemperature;
    private long? recoveryPower;
    public bool ControlConnectionAvailable { get; private set; }
    public CameraDescriptor Camera
    {
        get; private set;
    }
    public RecoveryOptions Options
    {
        get;
    }
    public IReadOnlyDictionary<int, Control> Controls { get; private set; } = new Dictionary<int, Control>();
    public string SdkVersion { get; private set; } = "unknown";
    public string Backend { get; private set; } = "sdk";
    public bool SupportsRetainedFrameReads { get; private set; }
    private double readRetryOverheadSeconds;
    public string? Serial => serial;
    public string Phase { get; private set; } = "Disconnected";
    public string? LastError
    {
        get; private set;
    }
    public int? LastSdkExposureState
    {
        get; private set;
    }
    public int? LastSdkErrorCode
    {
        get; private set;
    }
    public event Action<string>? Diagnostic;
    public CameraSession(CameraDescriptor camera, Func<HostClient> factory, RecoveryOptions? options = null, string? serial = null, Func<HostClient>? sdkFallbackFactory = null)
    {
        Camera = camera;
        this.factory = factory;
        this.sdkFallbackFactory = sdkFallbackFactory;
        this.serial = serial;
        Options = options ?? new();
        Options.Validate();
    }
    private void Log(string message)
    {
        var listeners = Diagnostic;
        if (listeners is null) return;
        foreach (Action<string> listener in listeners.GetInvocationList())
        {
            try { listener(message); }
            catch { /* A failed diagnostic subscriber must not interrupt recovery. */ }
        }
    }
    private void State(string phase)
    {
        Phase = phase;
        Log(phase);
    }
    private Task<Reply> Call(string method, object? p, CancellationToken token, double? timeout = null) =>
        (host ?? throw new IOException("Camera host is not connected")).CallAsync(method, p, TimeSpan.FromSeconds(timeout ?? Options.CommandTimeoutSeconds), token);
    public async Task ConnectAsync(CancellationToken token)
    {
        await operation.WaitAsync(token).ConfigureAwait(false);
        try
        {
            if (hasConnected && (host is null || requiresReconnect))
                await RestoreControlConnection(token).ConfigureAwait(false);
            else if (!hasConnected)
            {
                try { await OpenAsync(token).ConfigureAwait(false); }
                catch (Exception e) when (CanFallback && IsRecoverable(e))
                {
                    await ActivateFallback(e.Message, token).ConfigureAwait(false);
                    await OpenAsync(token).ConfigureAwait(false);
                }
                hasConnected = true;
                ControlConnectionAvailable = true;
                State("Idle");
            }
        }
        catch (Exception error) { Log($"Connection failed: {error.Message}"); KillHost(); throw; }
        finally { operation.Release(); }
    }
    private bool CanFallback => sdkFallbackFactory is not null && !usingFallback;
    private static bool IsRecoverable(Exception e) => e is IOException or TimeoutException
        && e is not InvalidDataException && e is not SdkException { Retryable: false };
    private async Task ActivateFallback(string reason, CancellationToken token)
    {
        KillHost();
        usingFallback = true;
        Log($"Switching to SDK fallback for this connection: {reason}");
        State("SDK fallback reconnect delay");
        await Task.Delay(TimeSpan.FromSeconds(Options.ReconnectDelaySeconds), token).ConfigureAwait(false);
    }
    private async Task OpenAsync(CancellationToken token)
    {
        if (hasConnected && serial is null)
            throw new InvalidOperationException("Automatic reconnection requires a camera serial number");
        lock (sync)
        {
            ObjectDisposedException.ThrowIf(disposed, this);
            host = usingFallback ? sdkFallbackFactory!() : factory();
            supervised = host.Supervised;
        }
        applied.Clear();
        Log($"Host process {host.ProcessId}; selected serial {serial ?? "initial selection"}");
        State("Opening");
        var result = (await Call("open", new
        {
            name = Camera.Name,
            serial,
            recovery = JsonSerializer.SerializeToElement(Options, new JsonSerializerOptions(JsonSerializerDefaults.Web)),
            allowSdkFallback = sdkFallbackFactory is not null,
            recoveryState = new { temperature = recoveryTemperature, power = recoveryPower, settle = requiresCoolingSettle }
        }, token).ConfigureAwait(false)).Result;
        var identity = result.GetProperty("serial");
        string? found = identity.ValueKind == JsonValueKind.String ? identity.GetString() : null;
        if (serial is not null && serial != found)
            throw new InvalidDataException("Camera identity changed");
        serial = found;
        var info = result.GetProperty("info");
        if (!info.GetProperty("formats").EnumerateArray().Any(v => v.GetInt32() == 2))
            throw new NotSupportedException("Camera does not support RAW16");
        var camera = CameraDescriptor.Parse(info);
        if (camera.Width != Camera.Width || camera.Height != Camera.Height || camera.Name != Camera.Name)
            throw new InvalidDataException("Camera geometry changed");
        Camera = camera;
        SdkVersion = result.GetProperty("sdkVersion").GetString()!;
        Backend = result.TryGetProperty("backend", out var backend) ? backend.GetString()! : "sdk";
        if (supervised) usingFallback = result.TryGetProperty("sdkFallback", out var fallback) && fallback.GetBoolean();
        SupportsRetainedFrameReads = Backend == "direct" && info.TryGetProperty("retainedFrameReads", out var retained) && retained.ValueKind == JsonValueKind.True;
        readRetryOverheadSeconds = info.TryGetProperty("readRetryOverheadSeconds", out var overhead) && overhead.TryGetDouble(out var overheadSeconds) && double.IsFinite(overheadSeconds)
            ? Math.Clamp(overheadSeconds, 0, 15) : 0;
        Log($"Camera opened using {Backend}{(usingFallback ? " fallback" : "")}; SDK/driver {SdkVersion}");
        Controls = result.GetProperty("controls").EnumerateArray().Select(c => new Control(c.GetProperty("type").GetInt32(), c.GetProperty("min").GetInt64(), c.GetProperty("max").GetInt64(), c.GetProperty("value").GetInt64(), c.GetProperty("writable").GetBoolean())).ToDictionary(c => c.Type);
        if (!Controls.ContainsKey(1))
            throw new NotSupportedException("Camera exposure control is unavailable");
        if (Backend == "direct" && CanFallback && Camera.Name is "ZWO ASI2600MM Duo" or "ZWO ASI220MM Mini")
            Controls = Controls.ToDictionary(k => k.Key, k => k.Key == 1 ? k.Value with { Max = 2_000_000_000 } : k.Value);
        if (Camera.Cooled && new[] { 8, 16, 17 }.Any(c => !Controls.ContainsKey(c)))
            throw new NotSupportedException("Cooled camera lacks readable temperature/target/enable controls required for recovery");
        lock (sync)
        {
            foreach (var c in Controls.Values)
            {
                observed[c.Type] = c.Value;
                // Persistent imaging/environment controls only: never replay reset, GPS, or auto controllers.
                if ((c.Writable || c.Type == 6) && new[] { 0, 2, 3, 4, 5, 6, 7, 9, 13, 14, 16, 17, 18, 19, 20, 21, 22, 23 }.Contains(c.Type))
                    desired.TryAdd(c.Type, c.Value);
            }
        }
    }
    public long Value(int control, long fallback = 0)
    {
        lock (sync)
            return desired.GetValueOrDefault(control, observed.GetValueOrDefault(control, fallback));
    }
    public void Set(int control, long value)
    {
        if (!Controls.TryGetValue(control, out var cap) || !cap.Writable)
            throw new NotSupportedException($"Control {control} unavailable");
        if (value < cap.Min || value > cap.Max)
            throw new ArgumentOutOfRangeException(nameof(value));
        lock (sync)
        {
            ObjectDisposedException.ThrowIf(disposed, this);
            desired[control] = value;
        }
    }
    private Dictionary<int, long> Snapshot()
    {
        lock (sync)
            return new(desired);
    }
    private async Task Apply(Dictionary<int, long> values, CancellationToken token)
    {
        foreach (var (c, v) in values.OrderBy(k => k.Key == 17 ? 100 : k.Key).ToArray())
        {
            if (applied.TryGetValue(c, out var previous) && previous == v)
                continue;
            // A direct fixed USB limit becomes writable on SDK fallback; preserve it.
            if (Controls.TryGetValue(c, out var fixedCap) && !fixedCap.Writable)
            {
                if (fixedCap.Value != v) throw new IOException($"Read-only control {c} changed during restore");
                applied[c] = v;
                continue;
            }
            await Call("set", new
            {
                control = c,
                value = v
            }, token).ConfigureAwait(false);
            var actual = (await Call("get", new
            {
                control = c
            }, token).ConfigureAwait(false)).Result.GetInt64();
            if (actual != v)
            {
                // Some SDK cameras clamp offset inside their advertised range (ASI220MM
                // reports min 0 but applies at least 200). Preserve the actual pedestal
                // in both recovery settings and this frame's metadata. Other controls,
                // especially cooling, must still restore exactly.
                if (Backend != "sdk" || c != 5 || !Controls.TryGetValue(c, out var cap) || actual < cap.Min || actual > cap.Max)
                    throw new IOException($"Control {c} read-back {actual} differs from requested {v}");
                Log($"SDK applied offset {actual} instead of requested {v}; retaining applied offset");
                values[c] = actual;
                lock (sync)
                {
                    observed[c] = actual;
                    // Do not overwrite a newer UI change made during this transaction.
                    if (desired.GetValueOrDefault(c) == v)
                        desired[c] = actual;
                }
            }
            applied[c] = actual;
        }
    }
    public async Task RefreshAsync(CancellationToken token)
    {
        if (!await operation.WaitAsync(0, token).ConfigureAwait(false))
            return;
        try
        {
            if (!hasConnected || disposed)
                return;
            if (host is null || requiresReconnect)
                await RestoreControlConnection(token).ConfigureAwait(false);
            await Apply(Snapshot(), token).ConfigureAwait(false);
            foreach (int c in new[] { 8, 15 })
                if (Controls.ContainsKey(c))
                {
                    long v = (await Call("get", new
                    {
                        control = c
                    }, token).ConfigureAwait(false)).Result.GetInt64();
                    lock (sync)
                        observed[c] = v;
                }
        }
        catch { KillHost(); throw; }
        finally { operation.Release(); }
    }
    // Re-establish control without taking a new exposure or consuming its retry budget.
    // Thermal settling remains required before the next capture.
    private async Task RestoreControlConnection(CancellationToken token)
    {
        State("Restoring camera controls");
        KillHost();
        await Task.Delay(TimeSpan.FromSeconds(Options.ReconnectDelaySeconds), token).ConfigureAwait(false);
        await OpenAsync(token).ConfigureAwait(false);
        await Apply(Snapshot(), token).ConfigureAwait(false);
        requiresReconnect = false;
        ControlConnectionAvailable = true;
        Log($"Camera controls restored using {Backend}; no replacement exposure taken");
        State("Idle");
    }
    private void ScheduleControlRecovery()
    {
        lock (sync)
        {
            if (disposed || !hasConnected || !requiresReconnect || !controlRecovery.IsCompleted) return;
            controlRecovery = Task.Run(async () => {
                // One bounded attempt now; normal telemetry may try again later.
                using var deadline = CancellationTokenSource.CreateLinkedTokenSource(shutdown.Token);
                deadline.CancelAfter(TimeSpan.FromSeconds(Options.ReconnectDelaySeconds +
                    (2 * Controls.Count + 4) * Options.CommandTimeoutSeconds));
                bool acquired = false;
                try {
                    // A foreground capture or telemetry transaction handles its own
                    // reconnect. Never queue a stale recovery behind a long exposure.
                    if (!await operation.WaitAsync(0, deadline.Token).ConfigureAwait(false)) return;
                    acquired = true;
                    if (!disposed && requiresReconnect)
                        await RestoreControlConnection(deadline.Token).ConfigureAwait(false);
                }
                catch (Exception error) {
                    if (acquired) KillHost();
                    if (!shutdown.IsCancellationRequested) {
                        LastError = error.Message;
                        Log($"Camera control recovery failed: {error.Message}");
                        State("Camera controls unavailable");
                    }
                }
                finally { if (acquired) operation.Release(); }
            });
        }
    }
    private async Task<double?> Temperature(CancellationToken token)
    {
        if (!Controls.ContainsKey(8))
            return null;
        long t = (await Call("get", new
        {
            control = 8
        }, token).ConfigureAwait(false)).Result.GetInt64();
        lock (sync)
            observed[8] = t;
        return t / 10.0;
    }
    private async Task<long?> CoolerPower(CancellationToken token)
    {
        if (!Controls.ContainsKey(15)) return null;
        long power = (await Call("get", new { control = 15 }, token).ConfigureAwait(false)).Result.GetInt64();
        lock (sync) observed[15] = power;
        return power;
    }
    public async Task<Frame> CaptureAsync(Exposure exposure, CancellationToken token)
    {
        await operation.WaitAsync(token).ConfigureAwait(false);
        try
        {
            if (!hasConnected)
                throw new InvalidOperationException("Connect first");
            if (supervised)
                return await CaptureSupervised(exposure, token).ConfigureAwait(false);
            Validate(exposure);
            var settings = Snapshot();
            double? prior = null;
            long? priorPower = null;
            lock (sync)
            {
                if (observed.TryGetValue(8, out var t))
                    prior = t / 10.0;
                if (observed.TryGetValue(15, out var power)) priorPower = power;
                prior = recoveryTemperature ?? prior;
                priorPower = recoveryPower ?? priorPower;
            }
            Exception? last = null;
            bool eligibleForRecapture = exposure.microseconds / 1e6 <= Options.MaximumRetryExposureSeconds;
            int retries = eligibleForRecapture ? Options.MaxRetries : 0;
            int downloadRetries = Options.ReadyFrameDownloadRetries;
            if (!eligibleForRecapture)
                Log($"Full recapture disabled: {exposure.microseconds / 1e6:G} s exposure exceeds {Options.MaximumRetryExposureSeconds:G} s threshold");
            for (int attempt = 0; attempt <= retries; attempt++)
            {
                token.ThrowIfCancellationRequested();
                try
                {
                    if (attempt > 0 || host is null || requiresReconnect)
                    {
                        State($"Reconnect delay (attempt {attempt + 1})");
                        KillHost();
                        await Task.Delay(TimeSpan.FromSeconds(Options.ReconnectDelaySeconds), token).ConfigureAwait(false);
                        await OpenAsync(token).ConfigureAwait(false);
                        await Apply(settings, token).ConfigureAwait(false);
                        if (settings.GetValueOrDefault(17) != 0 && prior.HasValue)
                            await Settle(prior.Value, priorPower, settings.GetValueOrDefault(16), token).ConfigureAwait(false);
                    }
                    else
                    {
                        await Apply(settings, token).ConfigureAwait(false);
                        if (requiresCoolingSettle && settings.GetValueOrDefault(17) != 0 && prior.HasValue)
                            await Settle(prior.Value, priorPower, settings.GetValueOrDefault(16), token).ConfigureAwait(false);
                        prior = await Temperature(token).ConfigureAwait(false) ?? prior;
                        if (settings.GetValueOrDefault(17) != 0)
                            priorPower = await CoolerPower(token).ConfigureAwait(false) ?? priorPower;
                    }
                    if (Backend == "direct" && CanFallback)
                    {
                        try { await Call("validate", exposure, token).ConfigureAwait(false); }
                        catch (SdkException e) when (e.Code == 8)
                        {
                            // No exposure has started: routing is not an exposure retry.
                            await ActivateFallback(e.Message, token).ConfigureAwait(false);
                            await OpenAsync(token).ConfigureAwait(false);
                            Validate(exposure);
                            await Apply(settings, token).ConfigureAwait(false);
                            if (settings.GetValueOrDefault(17) != 0 && prior.HasValue)
                                await Settle(prior.Value, priorPower, settings.GetValueOrDefault(16), token).ConfigureAwait(false);
                        }
                    }
                    requiresReconnect = requiresCoolingSettle = false;
                    recoveryTemperature = null;
                    recoveryPower = null;
                    ControlConnectionAvailable = true;
                    State("Starting exposure");
                    DateTime started = DateTime.UtcNow;
                    object parameters = Backend == "direct" ? new {
                        exposure.width, exposure.height, exposure.bin, exposure.x, exposure.y,
                        exposure.microseconds, exposure.dark,
                        readRetries = SupportsRetainedFrameReads || eligibleForRecapture ? Options.DirectReadRetries : 0,
                        transferTimeoutSeconds = Options.DownloadTimeoutSeconds,
                        captureTimeoutSeconds = ReadyTimeoutSeconds(exposure.microseconds / 1e6) + Options.CommandTimeoutSeconds
                    } : exposure;
                    await Call("start", parameters, token).ConfigureAwait(false);
                    State("Exposing");
                    var clock = Stopwatch.StartNew();
                    while (true)
                    {
                        LastSdkExposureState = (await Call("status", null, token).ConfigureAwait(false)).Result.GetInt32();
                        if (LastSdkExposureState == 2)
                            break;
                        if (LastSdkExposureState != 1)
                            throw new SdkException($"Exposure ended in SDK state {LastSdkExposureState}");
                        if (clock.Elapsed.TotalSeconds > ReadyTimeoutSeconds(exposure.microseconds / 1e6))
                            throw new TimeoutException("Exposure readiness deadline exceeded");
                        await Task.Delay(25, token).ConfigureAwait(false);
                    }
                    DateTime ended = DateTime.UtcNow;
                    State("Downloading");
                    Reply reply;
                    int transferRetry = 0;
                    while (true)
                    {
                        try
                        {
                            reply = await Call("download", null, token, Options.DownloadTimeoutSeconds).ConfigureAwait(false);
                            break;
                        }
                        catch (SdkException e)
                        {
                            LastError = e.Message;
                            LastSdkErrorCode = e.Code;
                            Log($"Transfer failure: {e.Message}");
                            if (!e.Retryable)
                                throw;
                            LastSdkExposureState = (await Call("status", null, token).ConfigureAwait(false)).Result.GetInt32();
                            Log($"Post-transfer SDK state: {LastSdkExposureState}");
                            if (transferRetry++ >= downloadRetries || LastSdkExposureState != 2)
                                throw;
                            State($"Rereading ready frame ({transferRetry}/{downloadRetries})");
                            await Task.Delay(TimeSpan.FromSeconds(Options.ReconnectDelaySeconds), token).ConfigureAwait(false);
                        }
                    }
                    if (reply.Result.GetProperty("width").GetInt32() != exposure.width || reply.Result.GetProperty("height").GetInt32() != exposure.height)
                        throw new InvalidDataException("Unexpected capture dimensions");
                    var pixels = new ushort[reply.Pixels.Length / 2];
                    Buffer.BlockCopy(reply.Pixels, 0, pixels, 0, reply.Pixels.Length);
                    int retainedReads = SupportsRetainedFrameReads && reply.Result.TryGetProperty("readRecoveries", out var reads)
                        ? reads.GetInt32() : 0;
                    if (retainedReads > 0)
                        Log($"Recovered retained frame after {retainedReads} transfer retries; no new exposure");
                    if (reply.Result.TryGetProperty("cleanupError", out var cleanup)) {
                        LastError = cleanup.GetString();
                        Log($"Frame preserved; reconnect required after cleanup failure: {LastError}");
                        KillHost();
                    }
                    if (attempt > 0 || transferRetry > 0)
                        Log($"Capture recovered using {Backend}: {attempt} replacement exposures, {transferRetry} ready-frame download retries; returning {exposure.width}x{exposure.height} image");
                    State("Idle");
                    return new(pixels, exposure.width, exposure.height, started, ended, attempt, exposure, settings)
                        { RetainedReadRecoveries = retainedReads };
                }
                catch (Exception e) when (IsRecoverable(e))
                {
                    if (e is SdkException sdk)
                        LastSdkErrorCode = sdk.Code;
                    last = e;
                    LastError = e.Message;
                    Log($"Attempt {attempt + 1}/{retries + 1} failed using {Backend}, phase {Phase}: {e.Message}");
                    if (attempt < retries)
                        Log($"Scheduling replacement exposure {attempt + 1}/{retries}: {exposure.microseconds / 1e6:G} s, {exposure.width}x{exposure.height}, bin {exposure.bin}; reconnect delay {Options.ReconnectDelaySeconds:G} s");
                    KillHost();
                    // Keep one shared retry budget. No automatic re-exposure above the duration limit.
                    if (Backend == "direct" && CanFallback && attempt < retries)
                    {
                        usingFallback = true;
                        Log($"Next permitted retry will use SDK fallback: {e.Message}");
                    }
                }
            }
            State("Error");
            throw new IOException($"Exposure failed after {retries + 1} attempts. {last?.Message}", last);
        }
        catch (OperationCanceledException) { if (!supervised || host?.IsAlive != true) KillHost(); State("Aborted"); throw; }
        catch (Exception error) { Log($"Capture failed; no image returned: {error.Message}"); if (!supervised || host?.IsAlive != true) KillHost(); State("Error"); throw; }
        finally { operation.Release(); ScheduleControlRecovery(); }
    }
    private async Task<Frame> CaptureSupervised(Exposure exposure, CancellationToken token)
    {
        // Production frontends share Rust recovery. The original transaction above
        // remains available for raw-worker diagnostics and its regression fixtures.
        Validate(exposure);
        if (host is null || requiresReconnect) await RestoreControlConnection(token).ConfigureAwait(false);
        var requested = Snapshot();
        try
        {
            token.ThrowIfCancellationRequested();
            // Drain each bounded supervisor reply before sending abort. Cancelling
            // a pipe read mid-message would force HostClient to kill the supervisor.
            await Apply(requested, CancellationToken.None).ConfigureAwait(false);
            token.ThrowIfCancellationRequested();
            await Call("start", exposure, CancellationToken.None).ConfigureAwait(false);
            int replacements = exposure.microseconds / 1e6 <= Options.MaximumRetryExposureSeconds ? Options.MaxRetries : 0;
            double budget = (replacements + 1) * (ReadyTimeoutSeconds(exposure.microseconds / 1e6) + Options.CoolingTimeoutSeconds +
                (Options.ReadyFrameDownloadRetries + 1) * (Options.DownloadTimeoutSeconds + Options.ReconnectDelaySeconds) +
                (2 * Controls.Count + 10) * Options.CommandTimeoutSeconds);
            var clock = Stopwatch.StartNew();
            while (true)
            {
                var status = (await Call("status", null, CancellationToken.None).ConfigureAwait(false)).Result;
                token.ThrowIfCancellationRequested();
                Backend = status.GetProperty("backend").GetString()!;
                usingFallback = status.GetProperty("sdkFallback").GetBoolean();
                if (status.TryGetProperty("environment", out var environment)) {
                    lock (sync) {
                        foreach (int control in new[] { 8, 15 })
                            if (environment.TryGetProperty(control.ToString(), out var value) && value.ValueKind == JsonValueKind.Number && value.TryGetInt64(out long reading))
                                observed[control] = reading;
                    }
                    ControlConnectionAvailable = status.GetProperty("controlConnectionAvailable").GetBoolean();
                }
                string phase = status.GetProperty("phase").GetString() ?? "Exposing";
                if (phase != Phase) State(phase);
                int state = status.GetProperty("state").GetInt32();
                if (state == 2) {
                    var snapshot = status.GetProperty("snapshot");
                    Controls = snapshot.GetProperty("controls").EnumerateObject().Select(p => p.Value).Select(c =>
                        new Control(c.GetProperty("type").GetInt32(), c.GetProperty("min").GetInt64(), c.GetProperty("max").GetInt64(), c.GetProperty("value").GetInt64(), c.GetProperty("writable").GetBoolean())).ToDictionary(c => c.Type);
                    SdkVersion = snapshot.GetProperty("sdkVersion").GetString()!;
                    ControlConnectionAvailable = snapshot.GetProperty("controlConnectionAvailable").GetBoolean();
                    LastError = snapshot.GetProperty("error").GetString();
                    LastSdkExposureState = snapshot.GetProperty("sdkExposureState").ValueKind == JsonValueKind.Number ? snapshot.GetProperty("sdkExposureState").GetInt32() : null;
                    LastSdkErrorCode = snapshot.GetProperty("sdkErrorCode").ValueKind == JsonValueKind.Number ? snapshot.GetProperty("sdkErrorCode").GetInt32() : null;
                    SupportsRetainedFrameReads = Backend == "direct" && snapshot.GetProperty("info").TryGetProperty("retainedFrameReads", out var retained) && retained.ValueKind == JsonValueKind.True;
                    break;
                }
                if (state != 1) throw new IOException(status.GetProperty("error").GetString() ?? "Exposure ended without an image");
                if (clock.Elapsed.TotalSeconds > budget) throw new TimeoutException("Camera recovery deadline exceeded");
                await Task.Delay(25, token).ConfigureAwait(false);
            }
            var reply = await Call("download", null, CancellationToken.None, Options.DownloadTimeoutSeconds).ConfigureAwait(false);
            token.ThrowIfCancellationRequested();
            if (reply.Result.GetProperty("width").GetInt32() != exposure.width || reply.Result.GetProperty("height").GetInt32() != exposure.height ||
                reply.Pixels.Length != checked(exposure.width * exposure.height * 2)) throw new InvalidDataException("Unexpected capture dimensions");
            var pixels = new ushort[reply.Pixels.Length / 2];
            Buffer.BlockCopy(reply.Pixels, 0, pixels, 0, reply.Pixels.Length);
            var controls = reply.Result.GetProperty("controls").EnumerateObject().ToDictionary(p => int.Parse(p.Name), p => p.Value.GetInt64());
            lock (sync) {
                foreach (var (key, value) in controls) {
                    observed[key] = value;
                    if (requested.TryGetValue(key, out var old) && desired.GetValueOrDefault(key) == old) {
                        desired[key] = value;
                        applied[key] = value;
                    }
                }
            }
            requiresReconnect = requiresCoolingSettle = false;
            State("Idle");
            return new(pixels, exposure.width, exposure.height, reply.Result.GetProperty("startedUtc").GetDateTimeOffset().UtcDateTime,
                reply.Result.GetProperty("endedUtc").GetDateTimeOffset().UtcDateTime, reply.Result.GetProperty("recoveries").GetInt32(), exposure, controls)
                { RetainedReadRecoveries = reply.Result.TryGetProperty("readRecoveries", out var reads) ? reads.GetInt32() : 0 };
        }
        catch
        {
            // Abort cooperatively so Rust can restore camera controls. Killing the
            // supervisor is reserved for an unresponsive or broken pipe.
            try { await Call("abort", null, CancellationToken.None).ConfigureAwait(false); }
            catch { KillHost(); }
            throw;
        }
    }
    public double ReadyTimeoutSeconds(double seconds) => seconds + Options.ExposureGraceSeconds +
        (Backend == "direct" ? (1 + (SupportsRetainedFrameReads || seconds <= Options.MaximumRetryExposureSeconds
            ? Options.DirectReadRetries : 0)) * Options.DownloadTimeoutSeconds + Options.DirectReadRetries * readRetryOverheadSeconds : 0);
    private async Task Settle(double prior, long? priorPower, double target, CancellationToken token)
    {
        State($"Restoring cooling near {prior:F1} C");
        var clock = Stopwatch.StartNew();
        int stable = 0;
        var setpointHold = new CoolingSetpointHold(target, Options.TemperatureToleranceC, Options.CoolingSampleSeconds);
        while (clock.Elapsed.TotalSeconds < Options.CoolingTimeoutSeconds)
        {
            double? current = await Temperature(token).ConfigureAwait(false);
            long? power = await CoolerPower(token).ConfigureAwait(false);
            // A cold sensor initially remains near its old temperature even when the SDK
            // restarts its regulator at 1%. Wait for output to recover as well, otherwise
            // thermal inertia lets three early readings pass before the sensor warms.
            bool outputRecovered = !priorPower.HasValue || priorPower <= 10 ||
                (power.HasValue && power >= priorPower - 10);
            // Remembered output can be transient cooldown demand. Once the restored
            // target is held for 30 seconds, a lower steady output is legitimate.
            bool atStableTarget = setpointHold.Observe(current, power, clock.Elapsed.TotalSeconds);
            bool nearPrior = current.HasValue && current.Value >= Math.Min(prior, target) - Options.TemperatureToleranceC && current.Value <= prior + Options.TemperatureToleranceC;
            stable = (nearPrior && outputRecovered) || atStableTarget ? stable + 1 : 0;
            Log($"Cooling recovery: temperature {current:F1} C (prior {prior:F1}), power {power}% (prior {priorPower}%), stable {stable}/{Options.CoolingStableSamples}, setpoint held {setpointHold.HeldSeconds:F1}s");
            if (stable >= Options.CoolingStableSamples) {
                Log($"Cooling recovered at {current:F1} C and {power}% power; restored setpoint {target:F1} C");
                return;
            }
            await Task.Delay(TimeSpan.FromSeconds(Options.CoolingSampleSeconds), token).ConfigureAwait(false);
        }
        throw new TimeoutException("Camera did not recover its prior cooling temperature and output");
    }
    private void Validate(Exposure e)
    {
        if (e.width <= 0 || e.height <= 0 || e.width % 8 != 0 || e.height % 2 != 0 || !Camera.Bins.Contains(e.bin) || e.x < 0 || e.y < 0 ||
            (long)(e.x + (long)e.width) * e.bin > Camera.Width || (e.y + (long)e.height) * e.bin > Camera.Height ||
            e.microseconds <= 0 || !Controls.TryGetValue(1, out var cap) || e.microseconds < cap.Min || e.microseconds > cap.Max)
            throw new ArgumentOutOfRangeException(nameof(e), "Exposure or ROI is outside camera capabilities");
    }
    private void KillHost()
    {
        lock (sync)
        {
            if (hasConnected && !requiresCoolingSettle) {
                recoveryTemperature = observed.TryGetValue(8, out var t) ? t / 10.0 : null;
                recoveryPower = observed.TryGetValue(15, out var p) ? p : null;
                requiresCoolingSettle = true;
            }
            requiresReconnect = hasConnected;
            ControlConnectionAvailable = false;
            host?.Dispose();
            host = null;
        }
    }
    public void Dispose()
    {
        HostClient? closing;
        lock (sync)
        {
            if (disposed) return;
            disposed = true;
            ControlConnectionAvailable = false;
            shutdown.Cancel();
            closing = host;
            host = null;
        }
        bool closed = false;
        if (closing is not null) {
            try {
                using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(2));
                closing.CallAsync("close", null, TimeSpan.FromSeconds(2), deadline.Token).GetAwaiter().GetResult();
                closed = true;
            } catch { /* Terminate active or unresponsive acquisition before cleanup. */ }
            finally { closing.Dispose(); }
        }
        // Disconnect immediately after Abort must not cancel the only path capable
        // of switching off a direct camera's last PWM output. Reopen for cleanup only.
        if (!closed && hasConnected && Backend == "direct" && Camera.Cooled && serial is not null) {
            try {
                using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(
                    Options.ReconnectDelaySeconds + 3 * Options.CommandTimeoutSeconds));
                Task.Delay(TimeSpan.FromSeconds(Options.ReconnectDelaySeconds), deadline.Token).GetAwaiter().GetResult();
                using var cleanup = factory();
                var timeout = TimeSpan.FromSeconds(Options.CommandTimeoutSeconds);
                cleanup.CallAsync("open", new { name = Camera.Name, serial }, timeout, deadline.Token).GetAwaiter().GetResult();
                cleanup.CallAsync("set", new { control = 17, value = 0 }, timeout, deadline.Token).GetAwaiter().GetResult();
                cleanup.CallAsync("close", null, timeout, deadline.Token).GetAwaiter().GetResult();
            } catch (Exception error) {
                LastError = error.Message;
                Log($"Could not disable cooling on disconnect: {error.Message}");
            }
        }
    }
}
