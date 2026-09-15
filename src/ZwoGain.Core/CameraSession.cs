using System.Diagnostics;
using System.Text.Json;

namespace ZwoGain.Core;

/// <summary>Survives host death. One transaction includes expose, transfer, restore and re-expose.</summary>
public sealed class CameraSession : IDisposable
{
    private readonly Func<HostClient> factory;
    private readonly Func<HostClient>? sdkFallbackFactory;
    private bool usingFallback;
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
    private void State(string phase)
    {
        Phase = phase;
        Diagnostic?.Invoke(phase);
    }
    private Task<Reply> Call(string method, object? p, CancellationToken token, double? timeout = null) =>
        (host ?? throw new IOException("Camera host is not connected")).CallAsync(method, p, TimeSpan.FromSeconds(timeout ?? Options.CommandTimeoutSeconds), token);
    public async Task ConnectAsync(CancellationToken token)
    {
        await operation.WaitAsync(token).ConfigureAwait(false);
        try
        {
            if (!hasConnected)
            {
                try { await OpenAsync(token).ConfigureAwait(false); }
                catch (Exception e) when (CanFallback && IsRecoverable(e))
                {
                    await ActivateFallback(e.Message, token).ConfigureAwait(false);
                    await OpenAsync(token).ConfigureAwait(false);
                }
                hasConnected = true;
                State("Idle");
            }
        }
        catch { KillHost(); throw; }
        finally { operation.Release(); }
    }
    private bool CanFallback => sdkFallbackFactory is not null && !usingFallback;
    private static bool IsRecoverable(Exception e) => e is IOException or TimeoutException
        && e is not InvalidDataException && e is not SdkException { Retryable: false };
    private async Task ActivateFallback(string reason, CancellationToken token)
    {
        KillHost();
        usingFallback = true;
        Diagnostic?.Invoke($"Switching to SDK fallback for this connection: {reason}");
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
        }
        applied.Clear();
        Diagnostic?.Invoke($"Host process {host.ProcessId}; selected serial {serial ?? "initial selection"}");
        State("Opening");
        var result = (await Call("open", new
        {
            name = Camera.Name,
            serial
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
        SupportsRetainedFrameReads = Backend == "direct" && info.TryGetProperty("retainedFrameReads", out var retained) && retained.ValueKind == JsonValueKind.True;
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
                Diagnostic?.Invoke($"SDK applied offset {actual} instead of requested {v}; retaining applied offset");
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
            if (!hasConnected || host is null)
                return;
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
            Validate(exposure);
            var settings = Snapshot();
            double? prior = null;
            long? priorPower = null;
            lock (sync)
            {
                if (observed.TryGetValue(8, out var t))
                    prior = t / 10.0;
                if (observed.TryGetValue(15, out var power)) priorPower = power;
            }
            Exception? last = null;
            bool eligibleForRecapture = exposure.microseconds / 1e6 <= Options.MaximumRetryExposureSeconds;
            int retries = eligibleForRecapture ? Options.MaxRetries : 0;
            int downloadRetries = Options.ReadyFrameDownloadRetries;
            if (!eligibleForRecapture)
                Diagnostic?.Invoke($"Full recapture disabled: {exposure.microseconds / 1e6:G} s exposure exceeds {Options.MaximumRetryExposureSeconds:G} s threshold");
            for (int attempt = 0; attempt <= retries; attempt++)
            {
                token.ThrowIfCancellationRequested();
                try
                {
                    if (attempt > 0 || host is null)
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
                    State("Starting exposure");
                    DateTime started = DateTime.UtcNow;
                    object parameters = Backend == "direct" ? new {
                        exposure.width, exposure.height, exposure.bin, exposure.x, exposure.y,
                        exposure.microseconds, exposure.dark,
                        readRetries = SupportsRetainedFrameReads || eligibleForRecapture ? Options.DirectReadRetries : 0
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
                        if (clock.Elapsed.TotalSeconds > exposure.microseconds / 1e6 + Options.ExposureGraceSeconds)
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
                            Diagnostic?.Invoke($"Transfer failure: {e.Message}");
                            if (!e.Retryable)
                                throw;
                            LastSdkExposureState = (await Call("status", null, token).ConfigureAwait(false)).Result.GetInt32();
                            Diagnostic?.Invoke($"Post-transfer SDK state: {LastSdkExposureState}");
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
                        Diagnostic?.Invoke($"Recovered retained frame after {retainedReads} transfer retries; no new exposure");
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
                    Diagnostic?.Invoke($"Attempt {attempt + 1}/{retries + 1}, phase {Phase}: {e.Message}");
                    KillHost();
                    // Keep one shared retry budget. No automatic re-exposure above the duration limit.
                    if (Backend == "direct" && CanFallback && attempt < retries)
                    {
                        usingFallback = true;
                        Diagnostic?.Invoke($"Next permitted retry will use SDK fallback: {e.Message}");
                    }
                }
            }
            State("Error");
            throw new IOException($"Exposure failed after {retries + 1} attempts. {last?.Message}", last);
        }
        catch (OperationCanceledException) { KillHost(); State("Aborted"); throw; }
        catch { KillHost(); State("Error"); throw; }
        finally { operation.Release(); }
    }
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
            Diagnostic?.Invoke($"Cooling recovery: temperature {current:F1} C (prior {prior:F1}), power {power}% (prior {priorPower}%), stable {stable}/{Options.CoolingStableSamples}, setpoint held {setpointHold.HeldSeconds:F1}s");
            if (stable >= Options.CoolingStableSamples)
                return;
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
            closing = host;
            host = null;
        }
        if (closing is null) return;
        // Normal disconnect lets the direct worker disable its host-regulated cooler.
        // Abort/failure still use immediate process termination via KillHost.
        try {
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(2));
            closing.CallAsync("close", null, TimeSpan.FromSeconds(2), deadline.Token).GetAwaiter().GetResult();
        } catch { /* An unresponsive worker must still be terminated. */ }
        finally { closing.Dispose(); }
    }
}
