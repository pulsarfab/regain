using System.ComponentModel.Composition;
using NINA.Core.Model;
using NINA.Core.Utility;
using NINA.Equipment.Interfaces.Mediator;
using NINA.Profile.Interfaces;
using NINA.Sequencer.Container;
using NINA.Sequencer.Interfaces;
using NINA.Sequencer.SequenceItem;
using NINA.Sequencer.Trigger;
using NINA.Sequencer.Utility;
using NINA.Sequencer.Validations;
using Newtonsoft.Json;
using Regain.Core;

namespace Regain.NINA;

[Export(typeof(ISequenceItem))]
[ExportMetadata("Name", "Recover Regain mount")]
[ExportMetadata("Description", "Home, return to the active target, restore tracking and settle guiding")]
[ExportMetadata("Icon", "HomeSVG")]
[ExportMetadata("Category", "PulsarFab regain")]
[JsonObject(MemberSerialization.OptIn)]
public sealed class RecoverMount : SequenceItem, IValidatable
{
    private readonly MountRecoveryService service;
    [ImportingConstructor] public RecoverMount(ITelescopeMediator telescope, IGuiderMediator guider, ISafetyMonitorMediator safety,
        IProfileService profiles, MountCentering centering) : this(new(telescope, guider, safety, profiles, centering)) { }
    internal RecoverMount(MountRecoveryService service) { this.service = service; }
    [JsonProperty] public MountRecoveryOptions Options { get; set; } = new();
    private bool reconciliationRequired;
    [JsonProperty] public bool ReconciliationRequired { get => reconciliationRequired || service.RequiresInspection; set => reconciliationRequired = value; }
    public string Outcome { get; private set; } = "Homes and moves the selected mount when executed.";
    public IList<string> Issues { get; set; } = [];
    internal bool AllowAfterInspection()
    {
        if (!service.ClearInspection()) return false;
        ReconciliationRequired = false; RaisePropertyChanged(nameof(ReconciliationRequired)); return true;
    }
    public bool Validate()
    {
        var errors = new List<string>();
        try { Options.Validate(); } catch (ArgumentException e) { errors.Add(e.Message); }
        if (service.Mount is null) errors.Add("Select a Regain ASCOM mount.");
        if (ItemUtility.RetrieveContextCoordinates(Parent) is null) errors.Add("Place this action inside a target with coordinates.");
        Issues = errors; return errors.Count == 0;
    }
    public override async Task Execute(IProgress<ApplicationStatus> progress, CancellationToken token)
    {
        var selected = service.Mount;
        bool attempted = false;
        try {
            if (ReconciliationRequired) throw new IOException("Inspect the previous mount recovery and explicitly clear its reconciliation fence first.");
            if (!Validate()) throw new IOException(string.Join("\n", Issues));
            var target = MountRecoveryService.Copy(ItemUtility.RetrieveContextCoordinates(Parent).Coordinates);
            ReconciliationRequired = true; RaisePropertyChanged(nameof(ReconciliationRequired));
            Outcome = "Recovering mount"; RaisePropertyChanged(nameof(Outcome));
            attempted = true;
            await service.RecoverAsync(target, Options with { }, progress, token).ConfigureAwait(false);
            ReconciliationRequired = false; Outcome = "Mount recovered";
        } catch {
            Outcome = "Recovery aborted. Tracking-off and stop requested; inspect mount. No park attempted.";
            // Triggers have no instruction retry policy of their own. Interrupt
            // the sequence explicitly so failure cannot fall through to imaging.
            RequestInterrupt(Parent);
            if (!attempted) {
                ReconciliationRequired = true;
                await service.AbortSelectedAsync(selected).ConfigureAwait(false);
            }
            throw;
        } finally { RaisePropertyChanged(nameof(Outcome)); RaisePropertyChanged(nameof(ReconciliationRequired)); }
    }
    internal static void RequestInterrupt(ISequenceContainer? parent)
    {
        // Interrupt cancels synchronously, then awaits the container's execution
        // task, which includes this action. Awaiting it here deadlocks NINA.
        var pending = (ItemUtility.GetRootContainer(parent) ?? parent)?.Interrupt();
        if (pending is not null) _ = pending.ContinueWith(task => Logger.Error(task.Exception!.GetBaseException()),
            CancellationToken.None, TaskContinuationOptions.OnlyOnFaulted, TaskScheduler.Default);
    }
    public override object Clone()
    {
        var copy = new RecoverMount(service) { Options = Options with { }, ReconciliationRequired = ReconciliationRequired };
        copy.CopyMetaData(this); return copy;
    }
}

[Export(typeof(ISequenceTrigger))]
[ExportMetadata("Name", "Recover Regain mount on tracking loss")]
[ExportMetadata("Description", "Before the next light exposure, home and recover a mount with unexpected tracking or pointing loss")]
[ExportMetadata("Icon", "HomeSVG")]
[ExportMetadata("Category", "PulsarFab regain")]
[JsonObject(MemberSerialization.OptIn)]
public sealed class RecoverMountOnTrackingLoss : SequenceTrigger, IValidatable
{
    private readonly RecoverMount action;
    private readonly MountRecoveryService service;
    private int runtimeTrigger;
    internal Task ValidationAbort { get; private set; } = Task.CompletedTask;
    [ImportingConstructor] public RecoverMountOnTrackingLoss(ITelescopeMediator telescope, IGuiderMediator guider, ISafetyMonitorMediator safety,
        IProfileService profiles, MountCentering centering) : this(new(telescope, guider, safety, profiles, centering)) { }
    internal RecoverMountOnTrackingLoss(MountRecoveryService service) { this.service = service; action = new(service); }
    [JsonProperty] public MountRecoveryOptions Options { get => action.Options; set => action.Options = value; }
    [JsonProperty] public bool ReconciliationRequired { get => action.ReconciliationRequired; set => action.ReconciliationRequired = value; }
    public IList<string> Issues { get => action.Issues; set => action.Issues = value; }
    public string Outcome => action.Outcome;
    public bool Validate()
    {
        action.AttachNewParent(Parent);
        bool valid = ReconciliationRequired || action.Validate();
        // NINA catches trigger validation failures and otherwise continues to
        // the exposure. Cancel the container before returning an invalid result.
        if (!valid) {
            RecoverMount.RequestInterrupt(Parent);
            // UI preflight validation must not stop equipment merely because
            // someone is editing a value. A runtime ShouldTrigger decision does
            // require the same stop-only failure path as Execute.
            if (Interlocked.Exchange(ref runtimeTrigger, 0) != 0) {
                ReconciliationRequired = true;
                ValidationAbort = service.AbortSelectedAsync(service.Mount);
                _ = ValidationAbort.ContinueWith(task => Logger.Error(task.Exception!.GetBaseException()), CancellationToken.None,
                    TaskContinuationOptions.OnlyOnFaulted | TaskContinuationOptions.ExecuteSynchronously, TaskScheduler.Default);
            }
        }
        return valid;
    }
    internal bool AllowAfterInspection() { bool allowed = action.AllowAfterInspection(); RaisePropertyChanged(nameof(ReconciliationRequired)); return allowed; }
    public override bool ShouldTrigger(ISequenceItem previousItem, ISequenceItem nextItem)
    {
        Interlocked.Exchange(ref runtimeTrigger, 0);
        bool Decision(bool result) { if (result) Interlocked.Exchange(ref runtimeTrigger, 1); return result; }
        if (nextItem is not IExposureItem { ImageType: "LIGHT" }) return false;
        var mount = service.Mount;
        if (mount is null) return Decision(ReconciliationRequired);
        try { mount.Health.Options = Options with { }; }
        catch (ArgumentException) { return Decision(true); } // Validate must stop imaging, not throw inside ShouldTrigger.
        return Decision(ReconciliationRequired || mount.Health.Fault is not null);
    }
    public override async Task Execute(ISequenceContainer context, IProgress<ApplicationStatus> progress, CancellationToken token)
    {
        Interlocked.Exchange(ref runtimeTrigger, 0);
        action.AttachNewParent(context);
        try { await action.Execute(progress, token).ConfigureAwait(false); }
        finally { RaisePropertyChanged(nameof(Outcome)); RaisePropertyChanged(nameof(ReconciliationRequired)); }
    }
    public override object Clone()
    {
        var copy = new RecoverMountOnTrackingLoss(service) { Options = Options with { }, ReconciliationRequired = ReconciliationRequired };
        copy.CopyMetaData(this); return copy;
    }
}
