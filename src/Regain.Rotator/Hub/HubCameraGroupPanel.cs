using System.Globalization;
using System.Text.Json;
using System.Windows;
using System.Windows.Controls;

namespace Regain.Hub;

public sealed partial class HubConfigurationWindow
{
    private readonly StackPanel cameraGroupPanel = new();
    private ComboBox? cameraGroup;
    private TextBlock? cameraGroupSummary, cameraGroupPolicy;
    private StackPanel? cameraGroupMembers;
    private Button? cameraGroupStart, cameraGroupRead, cameraGroupCancel;
    private readonly List<(Guid source, TextBox duration, CheckBox light)> cameraExposures = [];
    private JsonElement? cameraGroupObservation;
    private bool cameraGroupInspected, cameraGroupUnknown;
    private Guid? SelectedCameraGroup() => cameraGroup?.SelectedItem is ComboBoxItem selected ? (Guid)selected.Tag : null;
    private void RenderCameraGroups()
    {
        cameraGroupPanel.Children.Clear(); cameraExposures.Clear(); cameraGroupObservation = null; cameraGroupInspected = cameraGroupUnknown = false;
        var coordinator = session?.CameraGroups;
        if (coordinator is null) { cameraGroupPanel.Children.Add(new TextBlock { Text = "This host does not advertise camera group coordination." }); return; }
        cameraGroupPanel.Children.Add(new TextBlock { Text = "Capture separate images from saved camera members. Apply and reload edited definitions first, then read retained status before starting. Closing this window leaves an admitted capture running. A new operation replaces the group's retained images. Start spread measures host requests; hardware synchronization is not guaranteed.", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(4) });
        cameraGroup = new ComboBox { MinWidth = 300, Margin = new Thickness(4), Tag = "camera-group" };
        foreach (var group in coordinator.Groups) cameraGroup.Items.Add(new ComboBoxItem { Content = group.GetProperty("label").GetString(), Tag = group.GetProperty("id").GetGuid() });
        cameraGroupPanel.Children.Add(cameraGroup);
        cameraGroupPolicy = new TextBlock { TextWrapping = TextWrapping.Wrap, Margin = new Thickness(4), Tag = "camera-group-policy" }; cameraGroupPanel.Children.Add(cameraGroupPolicy);
        cameraGroupPanel.Children.Add(new TextBlock { Text = "Next capture: exposure duration and frame type per camera", Margin = new Thickness(4) });
        cameraGroupMembers = new StackPanel(); cameraGroupPanel.Children.Add(cameraGroupMembers);
        var actions = new WrapPanel(); cameraGroupPanel.Children.Add(actions);
        cameraGroupRead = SourceButton("Read retained camera status", async () => await Run(async () => {
            if (SelectedCameraGroup() is not Guid selected) return;
            try { ObserveCameraGroup(await coordinator.StatusAsync(selected, cancellation: lifetime.Token)); cameraGroupInspected = true; }
            catch (HubException error) when (error.Remote?.Code == "unavailable") {
                cameraGroupObservation = null; cameraGroupInspected = true;
                cameraGroupSummary!.Text = "No retained operation in this host revision. Starting is an explicit new capture.";
            }
            status.Text = "Retained camera status read. No equipment was connected or polled.";
        }));
        cameraGroupStart = SourceButton("Start camera group capture", async () => await Run(async () => {
            if (SelectedCameraGroup() is not Guid selected || !cameraGroupInspected || cameraGroupUnknown) return;
            var exposures = CameraGroupRequests(); session!.Changed(); preview.Clear();
            try { ObserveCameraGroup(await coordinator.StartAsync(selected, exposures, lifetime.Token)); }
            catch (Exception error) {
                if (HubGroupContract.UnknownStart(error)) cameraGroupUnknown = true;
                cameraGroupSummary!.Text = "Start did not return a confirmed operation. Read retained status. If the outcome is unknown, reload and inspect before issuing another capture."; throw;
            }
            status.Text = "Camera group admitted. Read retained status to inspect each member and completed image.";
        }));
        cameraGroupCancel = SourceButton("Cancel camera operation", async () => await Run(async () => {
            if (SelectedCameraGroup() is not Guid selected || cameraGroupObservation is not JsonElement observed) return;
            ObserveCameraGroup(await coordinator.CancelAsync(selected, observed.GetProperty("operation").GetGuid(), lifetime.Token));
            status.Text = "Cancellation requested using the saved group policy. Read retained status until terminal.";
        }));
        actions.Children.Add(cameraGroupRead); actions.Children.Add(cameraGroupStart); actions.Children.Add(cameraGroupCancel);
        cameraGroupSummary = new TextBlock { TextWrapping = TextWrapping.Wrap, Margin = new Thickness(8), Tag = "camera-group-summary" }; cameraGroupPanel.Children.Add(cameraGroupSummary);
        cameraGroup.SelectionChanged += (_, _) => {
            cameraGroupObservation = null; cameraGroupInspected = cameraGroupUnknown = false; cameraExposures.Clear(); cameraGroupMembers.Children.Clear();
            if (SelectedCameraGroup() is Guid selected) {
                var definition = coordinator.Groups.Single(g => g.GetProperty("id").GetGuid() == selected);
                var failure = definition.GetProperty("failurePolicy").GetString() == "continue" ? "continue healthy captures" : "abort acknowledged captures";
                var cancellation = definition.GetProperty("cancellationPolicy").GetString() == "leaveRunning" ? "leave captures running" : "abort acknowledged captures";
                cameraGroupPolicy.Text = "On member failure: " + failure + ". On cancellation or deadline: " + cancellation + ". Deadline: " + definition.GetProperty("timeoutSeconds") + " s. No automatic rollback.";
                foreach (var member in definition.GetProperty("members").EnumerateArray()) {
                    var source = member.GetGuid(); var row = new WrapPanel { Margin = new Thickness(4) };
                    var name = session!.SavedConfiguration!.Value.GetProperty("sources").EnumerateArray().Single(s => s.GetProperty("id").GetGuid() == source).GetProperty("label").GetString();
                    row.Children.Add(new TextBlock { Text = name, MinWidth = 240, VerticalAlignment = VerticalAlignment.Center });
                    var duration = new TextBox { Text = "1", Width = 100, Margin = new Thickness(4), Tag = "camera-group-duration" };
                    duration.TextChanged += (_, _) => Controls(); row.Children.Add(duration);
                    row.Children.Add(new TextBlock { Text = "s", VerticalAlignment = VerticalAlignment.Center });
                    var light = new CheckBox { Content = "Light", IsChecked = true, Margin = new Thickness(8), Tag = "camera-group-light" }; row.Children.Add(light);
                    cameraExposures.Add((source, duration, light)); cameraGroupMembers.Children.Add(row);
                }
                cameraGroupSummary.Text = "Read retained status before starting. Inspect and save any existing images before replacing them.";
            }
            Controls();
        };
        cameraGroup.SelectedIndex = cameraGroup.Items.Count == 0 ? -1 : 0;
        if (coordinator.Groups.Count == 0) cameraGroupSummary.Text = "Add a camera group in Configuration, then review, apply and reload.";
    }
    private IReadOnlyList<HubCameraMemberRequest> CameraGroupRequests()
    {
        return cameraExposures.Select(member => {
            if (!double.TryParse(member.duration.Text, NumberStyles.Float, CultureInfo.InvariantCulture, out var seconds) || double.IsNaN(seconds) || double.IsInfinity(seconds) || seconds < 0)
                throw new InvalidOperationException("Enter a finite, nonnegative exposure duration for each member");
            return new HubCameraMemberRequest(member.source, seconds, member.light.IsChecked == true);
        }).ToArray();
    }
    private void ObserveCameraGroup(JsonElement result) { cameraGroupObservation = result.Clone(); cameraGroupSummary!.Text = HubCameraGroups.Summary(result); }
    private void CameraGroupControls(bool editable)
    {
        var ready = !busy && editable && session?.CameraGroups is not null && SelectedCameraGroup().HasValue;
        if (cameraGroup is not null) cameraGroup.IsEnabled = !busy && editable;
        if (cameraGroupMembers is not null) cameraGroupMembers.IsEnabled = !busy && editable;
        if (cameraGroupRead is not null) cameraGroupRead.IsEnabled = ready;
        var terminal = cameraGroupObservation is not JsonElement observed || HubCameraGroups.Terminal(observed);
        var valid = false;
        if (ready) { try { CameraGroupRequests(); valid = true; } catch (InvalidOperationException) { } }
        if (cameraGroupStart is not null) cameraGroupStart.IsEnabled = ready && valid && cameraGroupInspected && !cameraGroupUnknown && terminal;
        if (cameraGroupCancel is not null) cameraGroupCancel.IsEnabled = ready && cameraGroupObservation.HasValue && !terminal;
    }
}
