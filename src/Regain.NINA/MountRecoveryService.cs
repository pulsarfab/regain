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
    internal MountRecoveryService(ITelescopeMediator telescope, IGuiderMediator guider, ISafetyMonitorMediator safety,
        IProfileService profiles, MountCentering centering) : this(telescope, guider,
            () => safety.GetDevice() is null || (safety.GetInfo().Connected && safety.GetInfo().IsSafe),
            () => profiles.ActiveProfile,
            centering.CenterVerifiedAsync, new MountRecoveryJournal(Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "PulsarFab", "Regain", "MountRecovery"))) { }
    internal MountRecoveryService(ITelescopeMediator telescope, IGuiderMediator guider, Func<bool> safe, Func<object> profile,
        Func<Coordinates, IProgress<ApplicationStatus>, CancellationToken, Task> center, MountRecoveryJournal? journal = null)
    { this.telescope = telescope; this.guider = guider; this.safe = safe; this.profile = profile; this.center = center; this.journal = journal; }
    internal RegainMount? Mount => telescope.GetDevice() as RegainMount;
    private RegainMount? recoveryMount;
    internal bool HasPendingRecovery => recoveryMount?.Recovering == true || Mount?.Recovering == true;
    internal bool RequiresInspection => journal is not null &&
        ((Mount is { } current && journal.Required(current.Id)) || (recoveryMount is { } previous && journal.Required(previous.Id)));
    internal bool ClearInspection()
    {
        if (HasPendingRecovery) return false;
        if (journal is null) return true;
        var selected = Mount;
        if (selected is null || (recoveryMount is not null && recoveryMount.Id != selected.Id)) return false;
        return journal.ClearAfterInspection(selected.Id);
    }
    internal async Task RecoverAsync(Coordinates target, MountRecoveryOptions options, IProgress<ApplicationStatus> progress, CancellationToken token)
    {
        options.Validate();
        target = Copy(target);
        var mount = Mount ?? throw new InvalidOperationException("Select a PulsarFab regain ASCOM mount in NINA first.");
        bool tracking = mount.Health.ExpectedTracking;
        // A stopped/parked session grants no authority to enable tracking or
        // move the mount. Recovery is only for previously tracking imaging.
        if (!tracking) { Logger.Info("Regain mount recovery skipped: tracking was intentionally disabled."); return; }
        var rate = mount.SavedTrackingRate ?? throw new IOException("Wait for a healthy tracking observation before attempting mount recovery.");
        object selectedProfile = profile();
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(token);
        deadline.CancelAfter(TimeSpan.FromSeconds(options.MotionTimeoutSeconds));
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
        async Task Work()
        {
            // Own the wrapper until the real operation terminates, even if a
            // cancellation/deadline makes the caller stop awaiting a vendor task.
            using var retainedOwnership = ownership;
            Guard();
            using var attempt = journal?.Begin(mount.Id);
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
            if (!mount.CanFindHome || !mount.CanSlew || !mount.CanSetTrackingEnabled)
                throw new IOException("Mount must support homing, coordinate slews and tracking control.");
            Guard();
            Report("Homing mount");
            if (!await telescope.FindHome(progress, recoveryToken).ConfigureAwait(false)) throw new IOException("Homing was not acknowledged.");
            Guard();
            while (!mount.AtHome || mount.Slewing) { await Task.Delay(250, recoveryToken).ConfigureAwait(false); Guard(); }
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
            attempt?.Complete();
            mount.Recovered(tracking); Report("Mount recovery complete");
        }
        var work = Task.Run(Work, CancellationToken.None);
        _ = work.ContinueWith(task => { _ = task.Exception; }, CancellationToken.None,
            TaskContinuationOptions.OnlyOnFaulted | TaskContinuationOptions.ExecuteSynchronously, TaskScheduler.Default);
        try { await work.WaitAsync(recoveryToken).ConfigureAwait(false); }
        catch (OperationCanceledException error) when (!token.IsCancellationRequested) {
            deadline.Cancel(); throw new TimeoutException("Mount recovery timed out. Inspect equipment before another operation; a vendor command may still be in flight.", error);
        }
        catch { deadline.Cancel(); throw; }
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
