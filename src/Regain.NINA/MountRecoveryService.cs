using NINA.Astrometry;
using NINA.Core.Model;
using NINA.Core.Utility;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.Mediator;
using NINA.Profile.Interfaces;
using Regain.Core;

namespace Regain.NINA;

internal sealed class MountRecoveryService
{
    private readonly ITelescopeMediator telescope;
    private readonly IGuiderMediator guider;
    private readonly Func<bool> safe;
    private readonly Func<object> profile;
    private readonly Func<Coordinates, IProgress<ApplicationStatus>, CancellationToken, Task> center;
    private readonly MountRecoveryJournal? journal;
    private readonly Func<IGreenSwampApi> greenSwampApi;
    internal MountRecoveryService(ITelescopeMediator telescope, IGuiderMediator guider, ISafetyMonitorMediator safety,
        IProfileService profiles, MountCentering centering) : this(telescope, guider,
            () => safety.GetDevice() is null || (safety.GetInfo().Connected && safety.GetInfo().IsSafe),
            () => profiles.ActiveProfile,
            centering.CenterVerifiedAsync, new MountRecoveryJournal(MountRecoveryJournal.DefaultDirectory)) { }
    internal MountRecoveryService(ITelescopeMediator telescope, IGuiderMediator guider, Func<bool> safe, Func<object> profile,
        Func<Coordinates, IProgress<ApplicationStatus>, CancellationToken, Task> center, MountRecoveryJournal? journal = null,
        Func<IGreenSwampApi>? greenSwampApi = null)
    { this.telescope = telescope; this.guider = guider; this.safe = safe; this.profile = profile; this.center = center; this.journal = journal;
        this.greenSwampApi = greenSwampApi ?? GreenSwampApi.Connect; }
    internal RegainMount? Mount => telescope.GetDevice() as RegainMount;
    private RegainMount? recoveryMount;
    internal bool HasPendingRecovery => recoveryMount?.Recovering == true || Mount?.Recovering == true;
    internal bool RequiresInspection => journal is not null &&
        ((Mount is { } current && journal.Required(current.Id)) || (recoveryMount is { } previous && journal.Required(previous.Id)));
    internal bool ClearInspection()
    {
        if (HasPendingRecovery) return false;
        var selected = Mount;
        if (selected is null || (recoveryMount is not null && recoveryMount.Id != selected.Id)) return false;
        if (!selected.StoppedForInspection) return false;
        if (journal is not null && !journal.ClearAfterInspection(selected.Id)) return false;
        selected.ClearRecoveryFailure(); return true;
    }
    internal async Task AbortSelectedAsync(RegainMount? selected)
    {
        if (selected is null || selected.Recovering) return;
        var ownership = selected.BeginRecovery();
        recoveryMount = selected;
        var halt = Task.Run(async () => {
            using var retained = ownership;
            MountRecoveryJournal.Attempt? marker = null;
            try {
                try { marker = journal?.HoldForAbort(selected.Id); }
                catch (Exception error) { Logger.Error("Regain could not create the abort inspection marker: " + error); }
                await MountRecoveryStop.RunAsync(selected, guider, greenSwampApi, Task.CompletedTask).ConfigureAwait(false);
            } finally { marker?.Dispose(); }
        });
        _ = halt.ContinueWith(task => Logger.Error(task.Exception!.GetBaseException()), CancellationToken.None,
            TaskContinuationOptions.OnlyOnFaulted | TaskContinuationOptions.ExecuteSynchronously, TaskScheduler.Default);
        try { await halt.WaitAsync(TimeSpan.FromSeconds(6)).ConfigureAwait(false); }
        catch (TimeoutException) { Logger.Error("Regain preflight abort/tracking-off is still pending. Inspect mount; no park attempted."); }
    }
    internal async Task RecoverAsync(Coordinates target, MountRecoveryOptions options, IProgress<ApplicationStatus> progress, CancellationToken token)
    {
        var mount = Mount ?? throw new InvalidOperationException("Select a PulsarFab regain ASCOM mount in NINA first.");
        bool tracking = mount.Health.ExpectedTracking;
        // A stopped/parked session grants no authority to enable tracking or
        // move the mount. Recovery is only for previously tracking imaging.
        if (!tracking) { Logger.Info("Regain mount recovery skipped: tracking was intentionally disabled."); return; }
        object? selectedProfile = null;
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(token);
        deadline.CancelAfter(TimeSpan.FromSeconds(180)); // Validate custom deadline inside the owned attempt.
        var recoveryToken = deadline.Token;
        void Guard()
        {
            recoveryToken.ThrowIfCancellationRequested();
            if (mount.IsDisposed || !ReferenceEquals(Mount, mount) || !ReferenceEquals(profile(), selectedProfile)) throw new IOException("Mount or NINA profile changed during recovery.");
            if (!safe()) throw new IOException("Selected safety monitor is unsafe or unavailable.");
            recoveryToken.ThrowIfCancellationRequested();
        }
        void Report(string status)
        {
            Logger.Info("Regain mount recovery: " + status);
            progress?.Report(new ApplicationStatus { Source = "PulsarFab regain", Status = status });
        }
        async Task Confirm(Func<bool> condition, string failure)
        {
            // NINA's ASCOM adapter caches getter results independently of writes.
            // Allow its telemetry to catch up, without replaying the command.
            long started = System.Diagnostics.Stopwatch.GetTimestamp();
            while (true) {
                Guard(); bool accepted = condition(); Guard();
                if (accepted) return;
                if (System.Diagnostics.Stopwatch.GetElapsedTime(started).TotalSeconds >= 5) throw new IOException(failure);
                await Task.Delay(250, recoveryToken).ConfigureAwait(false);
            }
        }
        var ownership = mount.BeginRecovery();
        recoveryMount = mount;
        MountRecoveryJournal.Attempt? attempt = null;
        var stopGate = new object();
        Task sensorOperation = Task.CompletedTask;
        Task? stop = null;
        var workDone = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        async Task Halt()
        {
            bool workerWasFinished = workDone.Task.IsCompleted;
            await MountRecoveryStop.RunAsync(mount, guider, greenSwampApi, sensorOperation).ConfigureAwait(false);
            if (!workerWasFinished) {
                await workDone.Task.ConfigureAwait(false);
                // A blocked vendor write may enable tracking or motion when it
                // eventually returns. Stop again after all recovery work ended.
                await MountRecoveryStop.RunAsync(mount, guider, greenSwampApi, sensorOperation).ConfigureAwait(false);
            }
        }
        Task Stop()
        {
            lock (stopGate) return stop ??= Halt();
        }
        async Task Work()
        {
            attempt = journal?.Begin(mount.Id);
            options.Validate();
            deadline.CancelAfter(TimeSpan.FromSeconds(options.MotionTimeoutSeconds));
            target = Copy(target);
            selectedProfile = profile();
            var rate = mount.SavedTrackingRate ?? throw new IOException("Wait for a healthy tracking observation before attempting mount recovery.");
            Guard();
            bool guided = guider.GetDevice() is IGuider { Connected: true } guide &&
                new[] { "Guiding", "LostLock", "Dithering", "Settling" }.Contains(guide.State, StringComparer.OrdinalIgnoreCase);
            bool guiderConnected = guider.GetDevice() is { Connected: true };
            Guard();
            Report("Stopping guiding");
            if (guiderConnected && !await guider.StopGuiding(recoveryToken).ConfigureAwait(false) && guided)
                throw new IOException("Could not confirm guiding stopped; mount has not been moved.");
            Guard();
            if (!mount.Connected) {
                Guard();
                Report("Reconnecting selected ASCOM mount");
                if (!await mount.Connect(recoveryToken).ConfigureAwait(false)) throw new IOException("Mount did not reconnect.");
            }
            Guard();
            if (mount.AtPark) throw new IOException("Mount reports parked. Inspect and unpark explicitly before recovery.");
            bool sensorHome = mount.IsGreenSwamp && options.GreenSwampAutoHome;
            if ((!sensorHome && !mount.CanFindHome) || !mount.CanSlew || !mount.CanSetTrackingEnabled)
                throw new IOException("Mount must support homing, coordinate slews and tracking control.");
            Guard();
            if (sensorHome) {
                lock (stopGate) {
                    Guard();
                    if (stop is not null) throw new IOException("Mount recovery has already been aborted.");
                    sensorOperation = GreenSwampAutoHome.RunAsync(mount, options, greenSwampApi, Guard, Report, recoveryToken);
                }
                await sensorOperation.ConfigureAwait(false);
            } else {
                // NINA's ASCOM adapter logs some FindHome errors and returns
                // normally. A pre-existing AtHome=true could otherwise let a
                // failed command pass as verified homing and start a return slew.
                if (mount.AtHome) throw new IOException("Mount already reports home; a fresh ASCOM homing result cannot be verified. Inspect before continuing.");
                Report("Homing mount through ASCOM FindHome");
                if (!await telescope.FindHome(progress, recoveryToken).ConfigureAwait(false)) throw new IOException("Homing was not acknowledged.");
                Guard();
                while (!mount.AtHome || mount.Slewing) { await Task.Delay(250, recoveryToken).ConfigureAwait(false); Guard(); }
            }
            Guard();
            RestoreRate(mount, rate);
            Guard();
            mount.TrackingEnabled = true;
            Guard();
            await Confirm(() => mount.TrackingEnabled, "Mount did not enable tracking after homing.").ConfigureAwait(false);
            Guard(); Report("Returning to saved sequence target");
            if (!await telescope.SlewToCoordinatesAsync(target, recoveryToken).ConfigureAwait(false)) throw new IOException("Return slew failed; it will not be replayed.");
            Guard(); Report("Plate-solving and centering saved target");
            await center(Copy(target), progress, recoveryToken).ConfigureAwait(false);
            Guard();
            RestoreRate(mount, tracking ? rate : TrackingRate.STOPPED);
            Guard();
            mount.TrackingEnabled = tracking;
            Guard();
            await Confirm(() => mount.TrackingEnabled == tracking && (!tracking || SameRate(mount.TrackingRate, rate)),
                "Mount did not restore the saved tracking state and rate.").ConfigureAwait(false);
            Guard();
            if (guided && tracking) {
                Report("Restoring guiding and waiting for NINA settling");
                if (!await guider.StartGuiding(false, progress, recoveryToken).ConfigureAwait(false)) throw new IOException("Guiding did not settle.");
            }
            Guard();
            if (mount.Slewing || mount.TrackingEnabled != tracking) throw new IOException("Mount state changed before recovery completed.");
            Guard();
        }
        var work = Task.Run(async () => { try { await Work().ConfigureAwait(false); } finally { workDone.TrySetResult(); } }, CancellationToken.None);
        var callerDone = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        async Task ReleaseOwnership()
        {
            try {
                try { await work.ConfigureAwait(false); } catch { /* Caller observes the failure. */ }
                await callerDone.Task.ConfigureAwait(false);
                Task? pendingStop;
                lock (stopGate) pendingStop = stop;
                if (pendingStop is not null) await pendingStop.ConfigureAwait(false);
            } finally { attempt?.Dispose(); ownership.Dispose(); }
        }
        var released = ReleaseOwnership();
        _ = released.ContinueWith(task => Logger.Error(task.Exception!.GetBaseException()), CancellationToken.None,
            TaskContinuationOptions.OnlyOnFaulted | TaskContinuationOptions.ExecuteSynchronously, TaskScheduler.Default);
        async Task StopBeforeReturning()
        {
            deadline.Cancel();
            try { await Stop().WaitAsync(TimeSpan.FromSeconds(6)).ConfigureAwait(false); }
            catch (TimeoutException) { Logger.Error("Regain mount abort/tracking-off is still pending; commands remain owned and recovery is fenced. No park attempted."); }
        }
        try {
            await work.WaitAsync(recoveryToken).ConfigureAwait(false);
            Guard(); Report("Mount recovery complete"); Guard();
            attempt?.Complete(); mount.Recovered(tracking);
        }
        catch (OperationCanceledException error) when (!token.IsCancellationRequested) {
            await StopBeforeReturning().ConfigureAwait(false);
            throw new TimeoutException("Mount recovery timed out and was aborted. Stop and tracking-off requested; inspect equipment if confirmation is unavailable. No park attempted.", error);
        }
        catch { await StopBeforeReturning().ConfigureAwait(false); throw; }
        finally {
            callerDone.TrySetResult();
            if (work.IsCompleted && (stop?.IsCompleted ?? true)) await released.ConfigureAwait(false);
        }
    }
    private static void RestoreRate(RegainMount mount, TrackingRate rate)
    {
        if (rate.TrackingMode == TrackingMode.Custom) {
            if (rate.CustomRightAscensionRate is not double ra || rate.CustomDeclinationRate is not double dec || !double.IsFinite(ra) || !double.IsFinite(dec))
                throw new IOException("Saved custom tracking rate is unavailable.");
            mount.SetCustomTrackingRate(ra, dec);
        } else mount.TrackingMode = rate.TrackingMode;
    }
    private static bool SameRate(TrackingRate a, TrackingRate b) => a.TrackingMode == b.TrackingMode &&
        Equal(a.CustomRightAscensionRate, b.CustomRightAscensionRate) && Equal(a.CustomDeclinationRate, b.CustomDeclinationRate);
    private static bool Equal(double? a, double? b) => a is null || b is null ? a == b : Math.Abs(a.Value - b.Value) <= 1e-6;
    internal static Coordinates Copy(Coordinates target)
    {
        if (target is null || !double.IsFinite(target.RA) || target.RA is < 0 or >= 24 || !double.IsFinite(target.Dec) || target.Dec is < -90 or > 90)
            throw new ArgumentException("A valid active sequence target is required; mount telemetry is never used as the recovery target.");
        return target.Transform(target.Epoch);
    }
}
