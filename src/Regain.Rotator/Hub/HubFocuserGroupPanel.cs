using System.Globalization;
using System.Text.Json;
using System.Windows;
using System.Windows.Controls;

namespace Regain.Hub;

public sealed partial class HubConfigurationWindow
{
    private readonly StackPanel focuserGroupPanel = new();
    private ComboBox? focuserGroup;
    private TextBox? groupTarget;
    private TextBlock? groupSummary;
    private Button? groupStart, groupRead, groupCancel;
    private JsonElement? groupObservation;
    private bool groupInspected, groupUnknown;
    private void RenderFocuserGroups()
    {
        focuserGroupPanel.Children.Clear(); groupObservation = null; groupInspected = groupUnknown = false;
        var coordinator = session?.FocuserGroups;
        if (coordinator is null) {
            focuserGroupPanel.Children.Add(new TextBlock { Text = "This host does not advertise focuser group coordination." }); return;
        }
        focuserGroupPanel.Children.Add(new TextBlock { Text = "Move several absolute focusers to calibrated targets. These controls use saved groups; apply and reload edited definitions first. Read retained status before starting. Closing this window leaves an admitted operation running. Cancel stops further group work; already moving focusers may continue. No automatic Halt or rollback is performed.", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(4) });
        focuserGroup = new ComboBox { MinWidth = 300, Margin = new Thickness(4), Tag = "focuser-group" };
        foreach (var group in coordinator.Groups) focuserGroup.Items.Add(new ComboBoxItem { Content = group.GetProperty("label").GetString(), Tag = group.GetProperty("id").GetGuid() });
        focuserGroupPanel.Children.Add(focuserGroup);
        var parameter = coordinator.TargetDescription;
        focuserGroupPanel.Children.Add(new TextBlock { Text = parameter.GetProperty("label").GetString() + " (" + parameter.GetProperty("units").GetString() + ")", Margin = new Thickness(4) });
        groupTarget = new TextBox { MinWidth = 120, MaxWidth = 280, HorizontalAlignment = HorizontalAlignment.Left, Margin = new Thickness(4), Tag = "group-target", ToolTip = parameter.GetProperty("description").GetString() };
        groupTarget.TextChanged += (_, _) => Controls(); focuserGroupPanel.Children.Add(groupTarget);
        var actions = new WrapPanel(); focuserGroupPanel.Children.Add(actions);
        groupRead = SourceButton("Read retained group status", async () => await Run(async () => {
            if (SelectedGroup() is not Guid selected) return;
            try {
                ObserveGroup(await coordinator.StatusAsync(selected, cancellation: lifetime.Token)); groupInspected = true;
                status.Text = "Retained group result read. No equipment was connected or polled.";
            } catch (HubException error) when (error.Remote?.Code == "unavailable") {
                groupObservation = null; groupInspected = true;
                groupSummary!.Text = "No retained operation in this host revision. Starting is an explicit new operation.";
            }
        }));
        groupStart = SourceButton("Start calibrated group move", async () => await Run(async () => {
            if (SelectedGroup() is not Guid selected || !groupInspected || groupUnknown) return;
            var target = coordinator.ParseTarget(selected, groupTarget.Text);
            session!.Changed(); preview.Clear();
            try { ObserveGroup(await coordinator.StartAsync(selected, target, lifetime.Token)); }
            catch (Exception error) {
                if (error is not HubException hub || hub.Failure is not (HubFailure.Remote or HubFailure.Busy or HubFailure.InvalidRequest) || hub.Remote?.Code is "uncertain" or "timeout" or "disconnected") groupUnknown = true;
                groupSummary!.Text = "Start did not return a confirmed operation. Read retained status. If the outcome is unknown, reload and inspect before issuing another move."; throw;
            }
            status.Text = "Group operation admitted. Read retained status to inspect each member and completion.";
        }));
        groupCancel = SourceButton("Cancel selected operation", async () => await Run(async () => {
            if (SelectedGroup() is not Guid selected || groupObservation is not JsonElement observed) return;
            ObserveGroup(await coordinator.CancelAsync(selected, observed.GetProperty("operation").GetGuid(), lifetime.Token));
            status.Text = "Cancellation requested. Read retained status until terminal. Already moving focusers may continue; no Halt was sent.";
        }));
        actions.Children.Add(groupRead); actions.Children.Add(groupStart); actions.Children.Add(groupCancel);
        groupSummary = new TextBlock { TextWrapping = TextWrapping.Wrap, Margin = new Thickness(8), Tag = "group-summary" }; focuserGroupPanel.Children.Add(groupSummary);
        focuserGroup.SelectionChanged += (_, _) => {
            groupObservation = null; groupInspected = groupUnknown = false;
            if (SelectedGroup() is Guid selected) {
                var definition = coordinator.Groups.Single(g => g.GetProperty("id").GetGuid() == selected);
                groupTarget.Text = definition.GetProperty("minimum").GetInt32().ToString(CultureInfo.InvariantCulture);
                groupSummary.Text = "Saved logical bounds " + definition.GetProperty("minimum") + "…" + definition.GetProperty("maximum") + ". Read retained status before starting.";
            }
            Controls();
        };
        focuserGroup.SelectedIndex = focuserGroup.Items.Count == 0 ? -1 : 0;
        if (coordinator.Groups.Count == 0) groupSummary.Text = "Add a focuser group in Configuration, then review, apply and reload.";
    }
    private Guid? SelectedGroup() => focuserGroup?.SelectedItem is ComboBoxItem selected ? (Guid)selected.Tag : null;
    private void ObserveGroup(JsonElement result) { groupObservation = result.Clone(); groupSummary!.Text = HubFocuserGroups.Summary(result); }
    private void FocuserGroupControls(bool editable)
    {
        var ready = !busy && editable && session?.FocuserGroups is not null && SelectedGroup().HasValue;
        if (focuserGroup is not null) focuserGroup.IsEnabled = !busy && editable;
        if (groupTarget is not null) groupTarget.IsEnabled = !busy && editable;
        if (groupRead is not null) groupRead.IsEnabled = ready;
        var terminal = groupObservation is not JsonElement observed || HubFocuserGroups.Terminal(observed);
        var targetValid = false;
        if (ready) { try { session!.FocuserGroups!.ParseTarget(SelectedGroup()!.Value, groupTarget!.Text); targetValid = true; } catch (InvalidOperationException) { } }
        if (groupStart is not null) groupStart.IsEnabled = ready && targetValid && groupInspected && !groupUnknown && terminal;
        if (groupCancel is not null) groupCancel.IsEnabled = ready && groupObservation.HasValue && !terminal;
    }
}
