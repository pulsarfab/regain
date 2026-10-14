using System.ComponentModel.Composition;
using NINA.Core.Model;
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
    [JsonProperty] public bool ReconciliationRequired { get; set; }
    public string Outcome { get; private set; } = "Homes and moves the selected mount when executed.";
    public IList<string> Issues { get; set; } = [];
    internal bool AllowAfterInspection()
    {
        if (service.Mount?.Recovering == true) return false;
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
        try {
            if (ReconciliationRequired) throw new IOException("Inspect the previous mount recovery and explicitly clear its reconciliation fence first.");
            if (!Validate()) throw new IOException(string.Join("\n", Issues));
            var target = MountRecoveryService.Copy(ItemUtility.RetrieveContextCoordinates(Parent).Coordinates);
            ReconciliationRequired = true; RaisePropertyChanged(nameof(ReconciliationRequired));
            Outcome = "Recovering mount"; RaisePropertyChanged(nameof(Outcome));
            await service.RecoverAsync(target, Options with { }, progress, token).ConfigureAwait(false);
            ReconciliationRequired = false; Outcome = "Mount recovered";
        } catch {
            Outcome = "Recovery stopped. Inspect mount before allowing another operation.";
            // Triggers have no instruction retry policy of their own. Interrupt
            // the sequence explicitly so failure cannot fall through to imaging.
            var root = ItemUtility.GetRootContainer(Parent);
            if (root is not null) await root.Interrupt().ConfigureAwait(false);
            else if (Parent is not null) await Parent.Interrupt().ConfigureAwait(false);
            throw;
        } finally { RaisePropertyChanged(nameof(Outcome)); RaisePropertyChanged(nameof(ReconciliationRequired)); }
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
    [ImportingConstructor] public RecoverMountOnTrackingLoss(ITelescopeMediator telescope, IGuiderMediator guider, ISafetyMonitorMediator safety,
        IProfileService profiles, MountCentering centering) : this(new(telescope, guider, safety, profiles, centering)) { }
    internal RecoverMountOnTrackingLoss(MountRecoveryService service) { this.service = service; action = new(service); }
    [JsonProperty] public MountRecoveryOptions Options { get => action.Options; set => action.Options = value; }
    [JsonProperty] public bool ReconciliationRequired { get => action.ReconciliationRequired; set => action.ReconciliationRequired = value; }
    public IList<string> Issues { get => action.Issues; set => action.Issues = value; }
    public string Outcome => action.Outcome;
    public bool Validate() { action.AttachNewParent(Parent); return ReconciliationRequired || action.Validate(); }
    internal bool AllowAfterInspection() { bool allowed = action.AllowAfterInspection(); RaisePropertyChanged(nameof(ReconciliationRequired)); return allowed; }
    public override bool ShouldTrigger(ISequenceItem previousItem, ISequenceItem nextItem)
    {
        if (nextItem is not IExposureItem { ImageType: "LIGHT" }) return false;
        var mount = service.Mount;
        if (mount is null) return ReconciliationRequired;
        mount.Health.Options = Options with { };
        return ReconciliationRequired || mount.Health.Fault is not null;
    }
    public override async Task Execute(ISequenceContainer context, IProgress<ApplicationStatus> progress, CancellationToken token)
    {
        action.AttachNewParent(Parent ?? context);
        try { await action.Execute(progress, token).ConfigureAwait(false); }
        finally { RaisePropertyChanged(nameof(Outcome)); RaisePropertyChanged(nameof(ReconciliationRequired)); }
    }
    public override object Clone()
    {
        var copy = new RecoverMountOnTrackingLoss(service) { Options = Options with { }, ReconciliationRequired = ReconciliationRequired };
        copy.CopyMetaData(this); return copy;
    }
}
