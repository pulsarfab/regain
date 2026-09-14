using System.Diagnostics;
using System.Text.Json;

namespace ZwoGain.Core;

/// <summary>Survives host death. One transaction includes expose, transfer, restore and re-expose.</summary>
public sealed class CameraSession : IDisposable
{
    private readonly Func<HostClient> factory;
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
    public CameraSession(CameraDescriptor camera, Func<HostClient> factory, RecoveryOptions? options = null, string? serial = null)
    {
        Camera = camera;
        this.factory = factory;
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
                await OpenAsync(token).ConfigureAwait(false);
                hasConnected = true;
                State("Idle");
            }
        }
        catch { KillHost(); throw; }
        finally { operation.Release(); }
    }
    private async Task OpenAsync(CancellationToken token)
    {
        if (hasConnected && serial is null)
            throw new InvalidOperationException("Automatic reconnection requires a camera serial number");
        lock (sync)
        {
            ObjectDisposedException.ThrowIf(disposed, this);
            host = factory();
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
        Controls = result.GetProperty("controls").EnumerateArray().Select(c => new Control(c.GetProperty("type").GetInt32(), c.GetProperty("min").GetInt64(), c.GetProperty("max").GetInt64(), c.GetProperty("value").GetInt64(), c.GetProperty("writable").GetBoolean())).ToDictionary(c => c.Type);
        if (!Controls.ContainsKey(1))
            throw new NotSupportedException("Camera exposure control is unavailable");
        if (Camera.Cooled && new[] { 8, 16, 17 }.Any(c => !Controls.ContainsKey(c)))
            throw new NotSupportedException("Cooled camera lacks readable temperature/target/enable controls required for recovery");
        lock (sync)
        {
            foreach (var c in Controls.Values)
            {
                observed[c.Type] = c.Value;
                // Persistent imaging/environment controls only: never replay reset, GPS, or auto controllers.
                if (c.Writable && new[] { 0, 2, 3, 4, 5, 6, 7, 9, 13, 14, 16, 17, 18, 19, 20, 21, 22, 23 }.Contains(c.Type))
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
            lock (sync)
            {
                if (observed.TryGetValue(8, out var t))
                    prior = t / 10.0;
            }
            Exception? last = null;
            bool eligibleForRetry = exposure.microseconds / 1e6 <= Options.MaximumRetryExposureSeconds;
            int retries = eligibleForRetry ? Options.MaxRetries : 0;
            int downloadRetries = eligibleForRetry ? Options.ReadyFrameDownloadRetries : 0;
            if (!eligibleForRetry)
                Diagnostic?.Invoke($"Automatic retries disabled: {exposure.microseconds / 1e6:G} s exposure exceeds {Options.MaximumRetryExposureSeconds:G} s threshold");
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
                            await Settle(prior.Value, token).ConfigureAwait(false);
                    }
                    else
                    {
                        await Apply(settings, token).ConfigureAwait(false);
                        prior = await Temperature(token).ConfigureAwait(false) ?? prior;
                    }
                    State("Starting exposure");
                    DateTime started = DateTime.UtcNow;
                    object parameters = Backend == "direct" ? new {
                        exposure.width, exposure.height, exposure.bin, exposure.x, exposure.y,
                        exposure.microseconds, exposure.dark, readRetries = eligibleForRetry ? Options.DirectReadRetries : 0
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
                            await Task.Delay(TimeSpan.FromSeconds(Options.ReconnectDelaySeconds), token).ConfigureAwait(false);
                        }
                    }
                    if (reply.Result.GetProperty("width").GetInt32() != exposure.width || reply.Result.GetProperty("height").GetInt32() != exposure.height)
                        throw new InvalidDataException("Unexpected capture dimensions");
                    var pixels = new ushort[reply.Pixels.Length / 2];
                    Buffer.BlockCopy(reply.Pixels, 0, pixels, 0, reply.Pixels.Length);
                    State("Idle");
                    return new(pixels, exposure.width, exposure.height, started, ended, attempt, exposure, settings);
                }
                catch (Exception e) when ((e is IOException or TimeoutException) && e is not SdkException { Retryable: false })
                {
                    if (e is SdkException sdk)
                        LastSdkErrorCode = sdk.Code;
                    last = e;
                    LastError = e.Message;
                    Diagnostic?.Invoke($"Attempt {attempt + 1}/{retries + 1}, phase {Phase}: {e.Message}");
                    KillHost();
                }
            }
            State("Error");
            throw new IOException($"Exposure failed after {retries + 1} attempts. {last?.Message}", last);
        }
        catch (OperationCanceledException) { KillHost(); State("Aborted"); throw; }
        catch { KillHost(); State("Error"); throw; }
        finally { operation.Release(); }
    }
    private async Task Settle(double prior, CancellationToken token)
    {
        State($"Restoring cooling near {prior:F1} C");
        var clock = Stopwatch.StartNew();
        int stable = 0;
        while (clock.Elapsed.TotalSeconds < Options.CoolingTimeoutSeconds)
        {
            double? current = await Temperature(token).ConfigureAwait(false);
            stable = current.HasValue && Math.Abs(current.Value - prior) <= Options.TemperatureToleranceC ? stable + 1 : 0;
            if (stable >= Options.CoolingStableSamples)
                return;
            await Task.Delay(TimeSpan.FromSeconds(Options.CoolingSampleSeconds), token).ConfigureAwait(false);
        }
        throw new TimeoutException("Camera did not recover its prior cooling temperature");
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
        lock (sync)
        {
            disposed = true;
            host?.Dispose();
            host = null;
        }
    }
}
