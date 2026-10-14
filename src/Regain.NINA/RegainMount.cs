using System.ComponentModel;
using System.ComponentModel.Composition;
using NINA.Astrometry;
using NINA.Core.Enum;
using NINA.Core.Utility;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.ViewModel;
using NINA.Equipment.Utility;
using NINA.Profile.Interfaces;
using Regain.Core;

namespace Regain.NINA;

[Export(typeof(IEquipmentProvider))]
public sealed class MountProvider : IEquipmentProvider<ITelescope>
{
    private readonly IProfileService profiles;
    [ImportingConstructor] public MountProvider(IProfileService profiles) { this.profiles = profiles; }
    public string Name => "PulsarFab regain";
    public IList<ITelescope> GetEquipment() => new ASCOMInteraction(profiles).GetTelescopes()
        .Where(device => device is not RegainMount).Select(device => (ITelescope)new RegainMount(device)).ToList();
}

/// <summary>Decorates NINA's public ASCOM adapter; never opens another serial port.</summary>
public sealed partial class RegainMount : BaseINPC, ITelescope, IDisposable
{
    private readonly ITelescope upstream;
    private readonly object gate = new();
    private CancellationTokenSource? polling;
    private Task? pollTask;
    private int commandedMotion, recovering;
    private int upstreamDisposed;
    private bool disposed, intentionalStop;
    internal MountRecoveryMonitor Health { get; }
    private TrackingRate? savedTrackingRate;
    internal TrackingRate? SavedTrackingRate { get { lock (gate) return savedTrackingRate; } }
    internal bool Recovering => Volatile.Read(ref recovering) != 0;
    internal bool IsDisposed { get { lock (gate) return disposed; } }
    internal RegainMount(ITelescope upstream, TimeProvider? clock = null)
    {
        this.upstream = upstream; Health = new(clock); upstream.PropertyChanged += Changed;
    }
    private void Changed(object? sender, PropertyChangedEventArgs e) => RaisePropertyChanged(e.PropertyName);
    public string Id => "Regain.Mount/" + Uri.EscapeDataString(upstream.Id);
    internal bool IsGreenSwamp => string.Equals(upstream.Id, "ASCOM.GS.Sky.Telescope", StringComparison.OrdinalIgnoreCase);
    public string Name => upstream.Name;
    public string DisplayName => "PulsarFab regain Mount — " + upstream.DisplayName;
    public string Category => "PulsarFab regain";
    public string Description => "ASCOM mount monitoring; homing and target recovery are opt-in NINA sequence operations.";
    public string DriverInfo => "Regain mount: " + (Recovering ? "recovering" : Health.Fault ?? "monitoring");
    public string DriverVersion => typeof(RegainMount).Assembly.GetName().Version!.ToString();
    public bool Connected => !disposed && upstream.Connected;
    public bool HasSetupDialog => upstream.HasSetupDialog;
    public IList<string> SupportedActions => upstream.SupportedActions;
    public async Task<bool> Connect(CancellationToken token)
    {
        lock (gate) ObjectDisposedException.ThrowIf(disposed, this);
        bool connected = await upstream.Connect(token).ConfigureAwait(false);
        if (connected) {
            lock (gate) {
                if (disposed || token.IsCancellationRequested) { upstream.Disconnect(); return false; }
                if (!Recovering) { intentionalStop = false; savedTrackingRate = null; Health.Intent(false); }
                if (pollTask is null || pollTask.IsCompleted || polling?.IsCancellationRequested == true) {
                    var previous = pollTask ?? Task.CompletedTask;
                    polling?.Dispose(); polling = new(); pollTask = PollAfter(previous, polling.Token);
                }
            }
        }
        return connected;
    }
    private async Task PollAfter(Task previous, CancellationToken token)
    {
        await previous.ConfigureAwait(false);
        if (!token.IsCancellationRequested) await Poll(token).ConfigureAwait(false);
    }
    private async Task Poll(CancellationToken token)
    {
        // Yield before any COM getter; connection does not synchronously wait on a poll.
        await Task.Yield();
        try { while (!token.IsCancellationRequested) { Observe(); await Task.Delay(1000, token).ConfigureAwait(false); } }
        catch (OperationCanceledException) when (token.IsCancellationRequested) { }
    }
    internal void Observe()
    {
        if (Recovering || disposed) return;
        var previous = Health.Fault;
        var reading = new MountObservation(false, false, false, double.NaN, double.NaN);
        Exception? failure = null;
        try {
            if (Connected) reading = new MountObservation(true, upstream.TrackingEnabled, upstream.Slewing, upstream.RightAscension, upstream.Declination);
            if (Health.Observe(reading, Volatile.Read(ref commandedMotion) != 0 || Volatile.Read(ref manualAxes) != 0 || intentionalStop)) {
                var rate = upstream.TrackingRate;
                // The adapter may return an older TrackingEnabled value while
                // its rate already reports stopped. Keep the last good rate.
                if (rate.TrackingMode != TrackingMode.Stopped && (rate.TrackingMode != TrackingMode.Custom ||
                    (rate.CustomRightAscensionRate is double ra && double.IsFinite(ra) && rate.CustomDeclinationRate is double dec && double.IsFinite(dec))))
                    lock (gate) { if (!Recovering && !disposed) savedTrackingRate = rate; }
            }
        } catch (Exception error) { failure = error; Health.Observe(new(false, false, false, double.NaN, double.NaN)); }
        if (previous is null && Health.Fault is not null)
            Logger.Warning($"Regain mount fault: {Health.Fault}; upstream={upstream.Id}; connected={reading.Connected}; tracking={reading.Tracking}; RA={reading.RightAscensionHours}; Dec={reading.DeclinationDegrees}; error={failure?.Message}");
        RaisePropertyChanged(nameof(DriverInfo));
    }
    public void Disconnect()
    {
        if (Recovering) throw new InvalidOperationException("Mount recovery is active; cancel the sequence first.");
        polling?.Cancel(); intentionalStop = true; Health.Intent(false); upstream.Disconnect();
    }
    public void SetupDialog()
    {
        if (Connected || Recovering) throw new InvalidOperationException("Disconnect before changing upstream mount settings.");
        upstream.SetupDialog();
    }
    public bool TrackingEnabled {
        get => upstream.TrackingEnabled;
        set {
            if (!Recovering) { intentionalStop = !value; Health.Intent(value); }
            upstream.TrackingEnabled = value;
        }
    }
    public TrackingMode TrackingMode {
        get => upstream.TrackingMode;
        set { if (!Recovering) { intentionalStop = value == TrackingMode.Stopped; Health.Intent(!intentionalStop); } upstream.TrackingMode = value; }
    }
    private async Task<T> Motion<T>(Func<Task<T>> operation, bool stopIntent = false)
    {
        if (!Recovering && stopIntent) { intentionalStop = true; Health.Intent(false); }
        Interlocked.Increment(ref commandedMotion);
        try { return await operation().ConfigureAwait(false); }
        finally { Interlocked.Decrement(ref commandedMotion); if (!Recovering) Health.ResetPositionBaseline(); }
    }
    private async Task Motion(Func<Task> operation, bool stopIntent = false) => await Motion(async () => { await operation().ConfigureAwait(false); return true; }, stopIntent).ConfigureAwait(false);
    public Task FindHome(CancellationToken token) => Motion(() => upstream.FindHome(token), true);
    public Task Park(CancellationToken token) => Motion(() => upstream.Park(token), true);
    public Task Unpark(CancellationToken token) => Motion(() => upstream.Unpark(token));
    public Task<bool> SlewToCoordinates(Coordinates coordinates, CancellationToken token) => Motion(() => upstream.SlewToCoordinates(coordinates, token));
    public Task<bool> SlewToAltAz(TopocentricCoordinates coordinates, CancellationToken token) => Motion(() => upstream.SlewToAltAz(coordinates, token));
    public Task<bool> MeridianFlip(Coordinates targetCoordinates, CancellationToken token) => Motion(() => upstream.MeridianFlip(targetCoordinates, token));
    public bool Sync(Coordinates coordinates) { try { return upstream.Sync(coordinates); } finally { if (!Recovering) Health.ResetPositionBaseline(); } }
    internal IDisposable BeginRecovery()
    {
        lock (gate) {
            ObjectDisposedException.ThrowIf(disposed, this);
            if (Interlocked.CompareExchange(ref recovering, 1, 0) != 0) throw new InvalidOperationException("A mount recovery is already in flight.");
            return new RecoveryScope(this);
        }
    }
    private sealed class RecoveryScope(RegainMount owner) : IDisposable
    {
        public void Dispose()
        {
            lock (owner.gate) { Volatile.Write(ref owner.recovering, 0); if (owner.disposed) owner.DisposeUpstream(); }
        }
    }
    internal void Recovered(bool tracking) { intentionalStop = !tracking; Health.Intent(tracking); }
    public void Dispose()
    {
        lock (gate) {
            if (disposed) return;
            disposed = true; polling?.Cancel(); upstream.PropertyChanged -= Changed;
            // Keep an in-flight vendor operation owned until it terminates.
            if (!Recovering) DisposeUpstream();
        }
    }
    private void DisposeUpstream() { if (Interlocked.Exchange(ref upstreamDisposed, 1) == 0) (upstream as IDisposable)?.Dispose(); }
}
