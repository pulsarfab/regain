using System.ComponentModel.Composition;
using Newtonsoft.Json;
using NINA.Core.Model;
using NINA.Sequencer.SequenceItem;
using NINA.Sequencer.Validations;
using Regain.Hub;

namespace Regain.NINA;

[ExportMetadata("Name", "Move Regain focuser group")]
[ExportMetadata("Description", "Move calibrated absolute focusers through the shared Regain host and report each member's result")]
[ExportMetadata("Icon", "FocuserSVG")]
[ExportMetadata("Category", "PulsarFab regain")]
[Export(typeof(ISequenceItem))]
[JsonObject(MemberSerialization.OptIn)]
public sealed class MoveHubFocuserGroup : SequenceItem, IValidatable
{
    private readonly Func<string, Guid, CancellationToken, Task<HubFocuserGroups>> attach;
    private int executing;
    private bool reconciliationRequired;
    private string configPath = "", groupLabel = "Select a saved group", outcome = "";
    private Guid instance, group;
    private int target;
    private IList<string> issues = [];
    [ImportingConstructor]
    public MoveHubFocuserGroup() : this((path, instance, token) => HubFocuserGroups.AttachAsync(HubEquipment.Executable, path, instance, token)) { }
    internal MoveHubFocuserGroup(Func<string, Guid, CancellationToken, Task<HubFocuserGroups>> attach) { this.attach = attach; }
    private MoveHubFocuserGroup(MoveHubFocuserGroup copy) : this(copy.attach)
    {
        CopyMetaData(copy); ConfigPath = copy.ConfigPath; InstanceId = copy.InstanceId; GroupId = copy.GroupId;
        GroupLabel = copy.GroupLabel; Target = copy.Target; reconciliationRequired = copy.reconciliationRequired;
    }
    [JsonProperty] public string ConfigPath { get => configPath; set { configPath = value; RaisePropertyChanged(); } }
    [JsonProperty] public Guid InstanceId { get => instance; set { instance = value; RaisePropertyChanged(); } }
    [JsonProperty] public Guid GroupId { get => group; set { group = value; RaisePropertyChanged(); } }
    [JsonProperty] public string GroupLabel { get => groupLabel; set { groupLabel = value; RaisePropertyChanged(); } }
    [JsonProperty] public int Target { get => target; set { target = value; RaisePropertyChanged(); } }
    // Keep an interrupted/failed step fenced in a saved sequence as well as in a
    // clone. Re-running or NINA's error retry must never replay an unknown move.
    [JsonProperty] public bool ReconciliationRequired { get => reconciliationRequired; private set { reconciliationRequired = value; RaisePropertyChanged(); } }
    public string Outcome { get => outcome; private set { outcome = value; RaisePropertyChanged(); } }
    public IList<string> Issues { get => issues; set { issues = value; RaisePropertyChanged(); } }
    public bool Validate()
    {
        var errors = new List<string>();
        if (string.IsNullOrWhiteSpace(ConfigPath) || !Path.IsPathFullyQualified(ConfigPath) || !File.Exists(ConfigPath)) errors.Add("Choose an existing Regain hub configuration.");
        if (InstanceId == Guid.Empty || GroupId == Guid.Empty) errors.Add("Choose a saved focuser group.");
        if (ReconciliationRequired) errors.Add("Inspect the retained group result and equipment state, then explicitly allow a new operation.");
        Issues = errors; return errors.Count == 0;
    }
    public override async Task Execute(IProgress<ApplicationStatus> progress, CancellationToken token)
    {
        token.ThrowIfCancellationRequested();
        if (Interlocked.CompareExchange(ref executing, 1, 0) != 0) throw new InvalidOperationException("This group step is already running");
        HubFocuserGroups? client = null; Guid? operation = null; var attempted = false;
        // Capture the selected identities/coordinate before any asynchronous work.
        var path = ConfigPath; var instance = InstanceId; var group = GroupId; var target = Target;
        try {
            if (!Validate()) throw new InvalidOperationException(string.Join("\n", Issues));
            Outcome = "Attaching to saved Regain focuser group";
            progress?.Report(new ApplicationStatus { Source = "PulsarFab regain", Status = Outcome });
            client = await attach(path, instance, token).ConfigureAwait(false);
            client.ParseTarget(group, target.ToString(System.Globalization.CultureInfo.InvariantCulture));
            // Persist the fence before dispatch. Success alone clears it; losing
            // the acknowledgement cannot make a sequence restart replay a move.
            attempted = true; ReconciliationRequired = true;
            var result = await client.StartAsync(group, target, token).ConfigureAwait(false);
            operation = result.GetProperty("operation").GetGuid();
            while (true) {
                Outcome = HubFocuserGroups.Summary(result);
                progress?.Report(new ApplicationStatus { Source = "PulsarFab regain", Status = Outcome });
                if (HubFocuserGroups.Terminal(result)) break;
                await Task.Delay(100, token).ConfigureAwait(false);
                result = await client.StatusAsync(group, operation, token).ConfigureAwait(false);
            }
            if (result.GetProperty("phase").GetString() != "complete") throw new IOException(Outcome + "\nInspect each member before another group move.");
            ReconciliationRequired = false;
        } catch (OperationCanceledException) when (token.IsCancellationRequested) {
            if (operation.HasValue && client is not null) {
                try {
                    using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(3));
                    await client.CancelAsync(group, operation.Value, deadline.Token).ConfigureAwait(false);
                    Outcome += "\nCancellation requested. Already moving focusers may continue; no Halt was sent.";
                } catch { Outcome += "\nCancellation could not be confirmed. Inspect retained status; do not repeat the move."; }
            } else if (attempted) Outcome = "Start outcome is unknown. Inspect the retained group result before another move.";
            throw;
        } catch {
            if (attempted) Outcome += "\nThe step requires explicit reconciliation before another operation. Use hub setup to inspect retained status and equipment.";
            throw;
        } finally { client?.Dispose(); Volatile.Write(ref executing, 0); }
    }
    internal bool Executing => Volatile.Read(ref executing) != 0;
    internal void SelectGroup(string path, Guid instance, Guid group, string label)
    {
        if (Executing) throw new InvalidOperationException("Wait for this step to finish before changing its group");
        ConfigPath = path; InstanceId = instance; GroupId = group; GroupLabel = label;
        // Changing a binding is not evidence that an interrupted move ended.
    }
    internal void AllowNewOperationAfterInspection()
    {
        if (Executing) throw new InvalidOperationException("Wait for this step to finish");
        ReconciliationRequired = false; Outcome = "A new operation was explicitly allowed after inspection."; Validate();
    }
    public override object Clone() => new MoveHubFocuserGroup(this);
    public override string ToString() => $"Category: {Category}, Item: {nameof(MoveHubFocuserGroup)}, Group: {GroupLabel}, Target: {Target}";
}
