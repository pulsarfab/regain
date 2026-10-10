using System.Windows;
using System.Windows.Controls;
using Regain.Rotator;

namespace Regain.Hub;

/// User-scoped saved chooser entries only; never attaches to a host or opens
/// equipment. Revision-checked removal cannot overwrite another editor's save.
public sealed class HubSelectionManagerWindow : Window
{
    private readonly HubSelectionStore store;
    private readonly string type;
    private Guid revision;
    private readonly ListBox choices = new() { MinHeight = 150, HorizontalContentAlignment = HorizontalAlignment.Stretch };
    private readonly Button remove = new() { Content = "Remove selected saved choice", Padding = new Thickness(14, 8, 14, 8), IsEnabled = false };
    private readonly TextBlock status = new() { TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 12, 0, 0) };
    internal HubSelectionManagerWindow(HubSelectionStore store, string type)
    {
        if (!HubSelection.Types.Contains(type, StringComparer.Ordinal)) throw new ArgumentException("Unsupported hub class", nameof(type));
        this.store = store; this.type = type;
        Title = "PulsarFab regain — saved hub outputs"; Width = 780; Height = 480; MinWidth = 620; MinHeight = 380;
        WindowStartupLocation = WindowStartupLocation.CenterOwner; SetupTheme.Apply(this);
        var panel = new DockPanel { Margin = new Thickness(24) }; Content = panel;
        var heading = new StackPanel { Margin = new Thickness(0, 0, 0, 16) }; DockPanel.SetDock(heading, Dock.Top); panel.Children.Add(heading);
        heading.Children.Add(new TextBlock { Text = "Manage saved " + type + " choices", FontSize = 24 });
        heading.Children.Add(new TextBlock { Text = "Removal changes the saved native chooser list. It leaves the shared hub configuration and connected clients running. Rescan equipment to refresh the choices; use output setup to add a choice again.", TextWrapping = TextWrapping.Wrap });
        var bottom = new StackPanel(); DockPanel.SetDock(bottom, Dock.Bottom); panel.Children.Add(bottom);
        var actions = new StackPanel { Orientation = Orientation.Horizontal }; bottom.Children.Add(actions);
        var reload = new Button { Content = "Reload saved choices", Padding = new Thickness(14, 8, 14, 8), Margin = new Thickness(0, 0, 8, 0) };
        actions.Children.Add(reload); actions.Children.Add(remove); bottom.Children.Add(status); panel.Children.Add(choices);
        ScrollViewer.SetHorizontalScrollBarVisibility(choices, ScrollBarVisibility.Disabled);
        choices.SelectionChanged += (_, _) => remove.IsEnabled = choices.SelectedItem is ListBoxItem { Tag: HubSelection };
        reload.Click += (_, _) => Reload();
        remove.Click += (_, _) => {
            if (choices.SelectedItem is not ListBoxItem { Tag: HubSelection choice }) return;
            try {
                var next = store.Remove(choice.InstanceId, choice.OutputId, revision);
                Render(next); status.Text = "Removed the saved choice. Hub outputs and connected clients are unchanged.";
            } catch {
                // Require explicit reload; never retry a lost/conflicting save.
                remove.IsEnabled = false;
                status.Text = "Could not remove the saved choice. Reload to reconcile the selection file before trying another change.";
            }
        };
        Loaded += (_, _) => Reload();
    }
    private void Reload()
    {
        remove.IsEnabled = false; choices.Items.Clear();
        try { Render(store.Load()); status.Text = choices.Items.Count == 0 ? "No saved choices of this class." : "Select a saved choice to remove."; }
        catch { status.Text = "Saved choices could not be read. Correct the selection file before changing it: " + store.Path; }
    }
    private void Render(HubSelections saved)
    {
        revision = saved.Revision;
        choices.Items.Clear();
        foreach (var binding in saved.Bindings.Where(b => b.DeviceType == type)) {
            var details = new StackPanel { Margin = new Thickness(4, 6, 4, 6) };
            details.Children.Add(new TextBlock { Text = binding.Label + (binding.Simulated ? " [SIMULATION]" : ""), FontWeight = FontWeights.SemiBold, TextWrapping = TextWrapping.Wrap });
            details.Children.Add(new TextBlock { Text = binding.ConfigPath, TextWrapping = TextWrapping.Wrap });
            details.Children.Add(new TextBlock { Text = "Output " + binding.OutputId.ToString("D") + "\nHub " + binding.InstanceId.ToString("D"), FontSize = 12, TextWrapping = TextWrapping.Wrap });
            choices.Items.Add(new ListBoxItem { Tag = binding.Copy(), Content = details, HorizontalContentAlignment = HorizontalAlignment.Stretch });
        }
        choices.SelectedItem = null; remove.IsEnabled = false;
    }
    public static void Show(Window owner, HubSelectionStore store, string type) =>
        new HubSelectionManagerWindow(store, type) { Owner = owner }.ShowDialog();
}
