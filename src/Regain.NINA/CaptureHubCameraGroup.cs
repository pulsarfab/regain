using System.Collections.ObjectModel;
using System.ComponentModel.Composition;
using System.Text.Json;
using Newtonsoft.Json;
using NINA.Core.Model;
using NINA.Core.Utility;
using NINA.Image.ImageData;
using NINA.Image.Interfaces;
using NINA.Profile.Interfaces;
using NINA.Sequencer.SequenceItem;
using NINA.Sequencer.Validations;
using Regain.Hub;

namespace Regain.NINA;

[JsonObject(MemberSerialization.OptIn)]
public sealed class HubCameraSequenceMember : BaseINPC
{
    private double durationSeconds = 1;
    private bool light = true;
    [JsonProperty] public Guid Source { get; set; }
    [JsonProperty] public string Label { get; set; } = "Camera";
    [JsonProperty] public double DurationSeconds { get => durationSeconds; set { durationSeconds = value; RaisePropertyChanged(); } }
    [JsonProperty] public bool Light { get => light; set { light = value; RaisePropertyChanged(); } }
    internal HubCameraSequenceMember Copy() => new() { Source = Source, Label = Label, DurationSeconds = DurationSeconds, Light = Light };
}

[ExportMetadata("Name", "Capture Regain camera group")]
[ExportMetadata("Description", "Capture separate images from saved cameras through the shared Regain host and save each completed member")]
[ExportMetadata("Icon", "CameraSVG")]
[ExportMetadata("Category", "PulsarFab regain")]
[Export(typeof(ISequenceItem))]
[JsonObject(MemberSerialization.OptIn)]
public sealed class CaptureHubCameraGroup : SequenceItem, IValidatable
{
    private readonly Func<string, Guid, CancellationToken, Task<HubCameraGroups>> attach;
    private readonly Func<IHubCameraGroupImages> writers;
    private int executing;
    private bool reconciliationRequired;
    private string configPath = "", groupLabel = "Select a saved group", outcome = "", saveDirectory = "";
    private Guid instance, group;
    private ObservableCollection<HubCameraSequenceMember> members = [];
    private IList<string> issues = [];

    [ImportingConstructor]
    public CaptureHubCameraGroup(IImageDataFactory images, IProfileService profiles) : this(
        (path, instance, token) => HubCameraGroups.AttachAsync(HubEquipment.Executable, path, instance, token),
        () => new HubCameraGroupImages(images, profiles)) { }
    internal CaptureHubCameraGroup(Func<string, Guid, CancellationToken, Task<HubCameraGroups>> attach, Func<IHubCameraGroupImages> writers)
    { this.attach = attach; this.writers = writers; }
    private CaptureHubCameraGroup(CaptureHubCameraGroup copy) : this(copy.attach, copy.writers)
    {
        CopyMetaData(copy); ConfigPath = copy.ConfigPath; InstanceId = copy.InstanceId; GroupId = copy.GroupId;
        GroupLabel = copy.GroupLabel; SaveDirectory = copy.SaveDirectory;
        Members = new(copy.Members.Select(m => m.Copy())); reconciliationRequired = copy.reconciliationRequired;
    }
    [JsonProperty] public string ConfigPath { get => configPath; set { configPath = value; RaisePropertyChanged(); } }
    [JsonProperty] public Guid InstanceId { get => instance; set { instance = value; RaisePropertyChanged(); } }
    [JsonProperty] public Guid GroupId { get => group; set { group = value; RaisePropertyChanged(); } }
    [JsonProperty] public string GroupLabel { get => groupLabel; set { groupLabel = value; RaisePropertyChanged(); } }
    // Empty uses the active NINA profile directory, frozen before admission.
    [JsonProperty] public string SaveDirectory { get => saveDirectory; set { saveDirectory = value; RaisePropertyChanged(); } }
    [JsonProperty(ObjectCreationHandling = ObjectCreationHandling.Replace)] public ObservableCollection<HubCameraSequenceMember> Members { get => members; set { members = value; RaisePropertyChanged(); } }
    [JsonProperty] public bool ReconciliationRequired { get => reconciliationRequired; private set { reconciliationRequired = value; RaisePropertyChanged(); } }
    public string Outcome { get => outcome; private set { outcome = value; RaisePropertyChanged(); } }
    public IList<string> Issues { get => issues; set { issues = value; RaisePropertyChanged(); } }
    internal bool Executing => Volatile.Read(ref executing) != 0;

    public bool Validate()
    {
        var errors = new List<string>();
        if (string.IsNullOrWhiteSpace(ConfigPath) || !Path.IsPathFullyQualified(ConfigPath) || !File.Exists(ConfigPath)) errors.Add("Choose an existing Regain hub configuration.");
        if (InstanceId == Guid.Empty || GroupId == Guid.Empty) errors.Add("Choose a saved camera group.");
        if (!string.IsNullOrWhiteSpace(SaveDirectory) && !Path.IsPathFullyQualified(SaveDirectory)) errors.Add("Use an absolute save directory or leave it empty for the NINA profile directory.");
        if (Members is null || Members.Count is < 1 or > 64) errors.Add("Choose the saved camera members.");
        else {
            if (Members.Any(m => m is null) || Members.Select(m => m.Source).Distinct().Count() != Members.Count) errors.Add("Camera members must be distinct.");
            foreach (var member in Members.Where(m => m is not null)) {
                try { _ = new HubCameraMemberRequest(member.Source, member.DurationSeconds, member.Light, true); }
                catch (ArgumentException) { errors.Add("Each camera needs a valid identity and finite nonnegative exposure duration."); }
            }
        }
        if (ReconciliationRequired) errors.Add("Inspect the retained camera result and saved files, then explicitly allow a new operation.");
        Issues = errors; return errors.Count == 0;
    }
    public override async Task Execute(IProgress<ApplicationStatus> progress, CancellationToken token)
    {
        token.ThrowIfCancellationRequested();
        if (Interlocked.CompareExchange(ref executing, 1, 0) != 0) throw new InvalidOperationException("This group step is already running");
        HubCameraGroups? client = null; Guid? operation = null; var attempted = false; var terminal = false;
        var path = ConfigPath; var instance = InstanceId; var group = GroupId; var directory = SaveDirectory;
        void Report(string text) { Outcome = text; progress?.Report(new ApplicationStatus { Source = "PulsarFab regain", Status = text }); }
        try {
            if (!Validate()) throw new InvalidOperationException(string.Join("\n", Issues));
            var demands = Members.Select(m => new HubCameraMemberRequest(m.Source, m.DurationSeconds, m.Light, true)).ToArray();
            var writer = writers();
            // NINA's writer mutates FileSaveInfo.FilePath. Freeze a separate copy
            // for every member before any asynchronous work or profile change.
            var settings = demands.Select(_ => writer.SaveSettings()).ToArray();
            var root = string.IsNullOrWhiteSpace(directory) ? settings[0].FilePath : directory;
            if (string.IsNullOrWhiteSpace(root) || !Path.IsPathFullyQualified(root)) throw new InvalidOperationException("Choose an absolute image directory in this step or the NINA profile.");
            Directory.CreateDirectory(root);
            // Verify write access before exposing. This probe contains no image.
            var probe = Path.Combine(root, ".regain-" + Guid.NewGuid().ToString("N"));
            using (new FileStream(probe, FileMode.CreateNew, FileAccess.Write, FileShare.None, 1, FileOptions.DeleteOnClose)) { }
            Report("Attaching to saved Regain camera group");
            client = await attach(path, instance, token).ConfigureAwait(false);
            var definition = client.Groups.Single(g => g.GetProperty("id").GetGuid() == group);
            if (!definition.GetProperty("members").EnumerateArray().Select(m => m.GetGuid()).SequenceEqual(demands.Select(m => m.Source)))
                throw new InvalidOperationException("Camera membership changed. Choose the saved group again before exposing.");
            token.ThrowIfCancellationRequested();
            attempted = true; ReconciliationRequired = true;
            var result = await client.StartAsync(group, demands, token).ConfigureAwait(false);
            operation = result.GetProperty("operation").GetGuid();
            while (true) {
                Report(HubCameraGroups.Summary(result));
                if (HubCameraGroups.Terminal(result)) { terminal = true; break; }
                await Task.Delay(100, token).ConfigureAwait(false);
                result = await client.StatusAsync(group, operation, token).ConfigureAwait(false);
            }
            var summary = HubCameraGroups.Summary(result); var saves = new List<string>(); var failed = result.GetProperty("phase").GetString() != "complete";
            var report = result.GetProperty("result");
            for (var i = 0; i < demands.Length && report.ValueKind != JsonValueKind.Null; i++) {
                token.ThrowIfCancellationRequested();
                var member = report.GetProperty("members")[i];
                if (member.GetProperty("image").ValueKind == JsonValueKind.Null) continue;
                var image = member.GetProperty("image");
                settings[i].FilePath = Path.Combine(root, "regain-" + operation.Value.ToString("N"));
                settings[i].FilePattern = image.GetProperty("source").GetGuid().ToString("N") + "-" + image.GetProperty("acquisition").GetGuid().ToString("N");
                try {
                    var saved = await writer.SaveAsync(client, group, operation.Value, demands[i].Source, settings[i], token).ConfigureAwait(false);
                    saves.Add($"{demands[i].Source}: saved {saved}");
                } catch (OperationCanceledException) when (token.IsCancellationRequested) { throw; }
                catch (Exception error) {
                    failed = true;
                    var reason = error switch {
                        HubException => "The exact retained image could not be read from the hub.",
                        UnauthorizedAccessException => "Access to the save directory was denied.",
                        NotSupportedException => "The image format is not supported by NINA.",
                        IOException io => io.Message,
                        _ => "NINA could not save this image."
                    };
                    saves.Add($"{demands[i].Source}: save failed: {reason}. Inspect retained status for this exact image.");
                }
                Report(summary + "\n" + string.Join("\n", saves));
            }
            if (failed) throw new IOException(Outcome + "\nInspect each camera and saved file before another capture.");
            ReconciliationRequired = false;
        } catch (OperationCanceledException) when (token.IsCancellationRequested) {
            if (operation.HasValue && client is not null && !terminal) {
                try {
                    using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(3));
                    await client.CancelAsync(group, operation.Value, deadline.Token).ConfigureAwait(false);
                    Outcome += "\nCancellation requested using the saved group policy. Inspect retained status and files.";
                } catch { Outcome += "\nCancellation could not be confirmed. Inspect retained status; do not repeat the capture."; }
            } else if (attempted && !operation.HasValue) Outcome = "Start outcome is unknown. Inspect retained status before another capture.";
            else if (terminal) Outcome += "\nSaving was interrupted. Existing files remain; inspect retained images before another capture.";
            throw;
        } catch {
            if (attempted) Outcome += "\nExplicit reconciliation is required before another operation.";
            throw;
        } finally { client?.Dispose(); Volatile.Write(ref executing, 0); }
    }
    internal void SelectGroup(string path, Guid instance, JsonElement definition)
    {
        if (Executing) throw new InvalidOperationException("Wait for this step to finish before changing its group");
        ConfigPath = path; InstanceId = instance; GroupId = definition.GetProperty("id").GetGuid(); GroupLabel = definition.GetProperty("label").GetString()!;
        var previous = Members.ToDictionary(m => m.Source);
        Members = new(definition.GetProperty("members").EnumerateArray().Select(m => previous.TryGetValue(m.GetGuid(), out var old) ? old.Copy() :
            new HubCameraSequenceMember { Source = m.GetGuid(), Label = m.GetGuid().ToString() }));
    }
    internal void AllowNewOperationAfterInspection()
    {
        if (Executing) throw new InvalidOperationException("Wait for this step to finish");
        ReconciliationRequired = false; Outcome = "A new capture was explicitly allowed after inspection."; Validate();
    }
    public override object Clone() => new CaptureHubCameraGroup(this);
    public override string ToString() => $"Category: {Category}, Item: {nameof(CaptureHubCameraGroup)}, Group: {GroupLabel}";
}
