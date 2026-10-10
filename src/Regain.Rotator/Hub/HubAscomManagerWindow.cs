using System.IO;
using System.Windows;
using System.Windows.Controls;
using Microsoft.Win32;
using Regain.Rotator;

namespace Regain.Hub.ASCOM;

/// Reads saved choices and machine inventory without opening a host/equipment.
/// Only the explicit selector/editor actions attach to the shared host.
public sealed class HubAscomManagerWindow : Window
{
    private readonly HubSelectionStore store;
    private readonly string directory;
    private readonly string owner = HubAscomRegistration.CurrentOwner;
    private readonly Func<HubRegisteredOutput[]> inventory;
    private readonly Func<HubRegistrationRequest, Task<int>> execute;
    private readonly ListBox list = new() { MinHeight = 180, HorizontalContentAlignment = HorizontalAlignment.Stretch };
    private readonly TextBlock status = new() { TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 12, 0, 0) };
    private readonly Button register = Button("Register / refresh in ASCOM"), remove = Button("Remove ASCOM registration"),
        forget = Button("Remove saved choice"), edit = Button("Edit shared configuration"), reload = Button("Reload inventory");
    private readonly List<Button> additions = new();
    private Guid revision;
    private bool busy, closed, reconciled;
    private sealed class Choice(Guid id, HubSelection? saved, HubRegisteredOutput[] installed)
    {
        internal Guid Id { get; } = id;
        internal HubSelection? Saved { get; } = saved?.Copy();
        internal HubRegisteredOutput[] Installed { get; } = installed;
        internal HubSelection Binding => Saved ?? Installed[0].Selection;
    }
    internal HubAscomManagerWindow(string directory, HubSelectionStore store,
        Func<HubRegisteredOutput[]> inventory, Func<HubRegistrationRequest, Task<int>> execute)
    {
        this.directory = directory; this.store = store; this.inventory = inventory; this.execute = execute;
        Title = "PulsarFab regain — ASCOM hub outputs"; Width = 920; Height = 650; MinWidth = 760; MinHeight = 520;
        WindowStartupLocation = WindowStartupLocation.CenterScreen; SetupTheme.Apply(this);
        var panel = new DockPanel { Margin = new Thickness(24) }; Content = panel;
        var heading = new StackPanel { Margin = new Thickness(0, 0, 0, 16) }; DockPanel.SetDock(heading, Dock.Top); panel.Children.Add(heading);
        heading.Children.Add(new TextBlock { Text = "ASCOM hub outputs", FontSize = 26 });
        heading.Children.Add(new TextBlock { Text = "Choose a shared output, then register it for the ASCOM Chooser. Registration requests Windows administrator approval using your current account. Output identities remain stable when labels change.", TextWrapping = TextWrapping.Wrap });
        var add = new WrapPanel(); heading.Children.Add(add);
        foreach (var type in HubSelection.Types) {
            var button = Button("Choose " + type + " output"); additions.Add(button); add.Children.Add(button);
            button.Click += (_, _) => {
                if (busy || !reconciled) return;
                try { HubSelectionWindow.Select(Path.Combine(directory, "regain-alpaca.exe"), store, type); Reload(); }
                catch { Invalidate("Could not open output setup. Reload saved choices before another change."); }
            };
        }
        var bottom = new StackPanel(); DockPanel.SetDock(bottom, Dock.Bottom); panel.Children.Add(bottom);
        var actions = new WrapPanel(); bottom.Children.Add(actions);
        foreach (var button in new[] { register, remove, edit, forget, reload }) actions.Children.Add(button);
        bottom.Children.Add(status); panel.Children.Add(list);
        ScrollViewer.SetHorizontalScrollBarVisibility(list, ScrollBarVisibility.Disabled);
        list.SelectionChanged += (_, _) => Controls();
        reload.Click += (_, _) => { if (!busy) Reload(); };
        register.Click += async (_, _) => {
            if (!register.IsEnabled || list.SelectedItem is not ListBoxItem { Tag: Choice choice } || choice.Saved is null) return;
            await Change(HubRegistrationRequest.Register(store, revision, choice.Saved, owner));
        };
        remove.Click += async (_, _) => {
            if (!remove.IsEnabled || list.SelectedItem is not ListBoxItem { Tag: Choice choice }) return;
            await Change(HubRegistrationRequest.Remove(choice.Id, owner));
        };
        forget.Click += (_, _) => {
            if (!forget.IsEnabled || list.SelectedItem is not ListBoxItem { Tag: Choice choice } || choice.Saved is null) return;
            try { store.Remove(choice.Saved.InstanceId, choice.Saved.OutputId, revision); Reload();
                status.Text = "Removed the saved choice. Connected clients retain their current output."; }
            catch { Invalidate("Saved choices changed or could not be updated. Reload before another action."); }
        };
        edit.Click += (_, _) => {
            if (!edit.IsEnabled || list.SelectedItem is not ListBoxItem { Tag: Choice choice } || choice.Saved is null) return;
            try { HubConfigurationWindow.Show(this, Path.Combine(directory, "regain-alpaca.exe"), choice.Saved.ConfigPath, choice.Saved.InstanceId); Reload(); }
            catch { Invalidate("Could not open shared setup. Reload before another action."); }
        };
        Loaded += (_, _) => Reload(); Closed += (_, _) => closed = true;
        Controls();
    }
    private static Button Button(string text) => new() { Content = text, Padding = new Thickness(12, 8, 12, 8), Margin = new Thickness(0, 8, 8, 0) };
    private bool Owned(Choice choice) => choice.Installed.All(item => item.OwnerSid == owner &&
        string.Equals(Path.GetFullPath(item.Directory), Path.GetFullPath(directory), StringComparison.OrdinalIgnoreCase) &&
        string.Equals(Path.GetFullPath(item.BindingsPath), store.Path, StringComparison.OrdinalIgnoreCase) &&
        item.Version <= typeof(HubAscomRegistration).Assembly.GetName().Version);
    private void Controls()
    {
        var available = !busy && reconciled;
        var choice = (list.SelectedItem as ListBoxItem)?.Tag as Choice;
        register.IsEnabled = available && choice?.Saved is not null && Owned(choice);
        remove.IsEnabled = available && choice?.Installed.Length > 0 && Owned(choice);
        edit.IsEnabled = available && choice?.Saved is not null;
        // Remove registration first so an orphan cannot be created accidentally.
        forget.IsEnabled = available && choice?.Saved is not null && choice.Installed.Length == 0;
        reload.IsEnabled = !busy; list.IsEnabled = !busy;
        foreach (var button in additions) button.IsEnabled = available;
    }
    private void Invalidate(string text) { reconciled = false; status.Text = text; Controls(); }
    private void Reload()
    {
        list.Items.Clear(); reconciled = false;
        try {
            var saved = store.Load(); var installed = inventory(); revision = saved.Revision;
            var ids = saved.Bindings.Select(OutputIdentity.ClassId).Concat(installed.Select(i => i.ClassId)).Distinct();
            foreach (var id in ids) {
                var choice = new Choice(id, saved.Bindings.FirstOrDefault(b => OutputIdentity.ClassId(b) == id), installed.Where(i => i.ClassId == id).ToArray());
                var binding = choice.Binding;
                var state = choice.Installed.Length == 0 ? "Not registered" : !Owned(choice) ? "Recorded for another owner, install, selection file or newer version" :
                    choice.Installed.Length == 2 && choice.Installed.All(i => i.Phase == "ready") ? "Recorded in both ASCOM registry views" : "Partial registration — explicitly refresh or remove";
                if (choice.Saved is null) state += "; saved choice missing";
                var row = new StackPanel { Margin = new Thickness(4, 6, 4, 6) };
                row.Children.Add(new TextBlock { Text = binding.Label + (binding.Simulated ? " [SIMULATION]" : ""), FontWeight = FontWeights.SemiBold, TextWrapping = TextWrapping.Wrap });
                row.Children.Add(new TextBlock { Text = binding.DeviceType + " · " + state, TextWrapping = TextWrapping.Wrap });
                row.Children.Add(new TextBlock { Text = binding.ConfigPath + "\nOutput " + binding.OutputId.ToString("D") + " · Hub " + binding.InstanceId.ToString("D"), FontSize = 12, TextWrapping = TextWrapping.Wrap });
                list.Items.Add(new ListBoxItem { Tag = choice, Content = row });
            }
            reconciled = true; status.Text = "Registration edits leave connected clients running. Remove registration before removing a saved choice. Registry records are not a live equipment check.";
        } catch { status.Text = "Could not read saved choices or registration inventory. Correct the files/registry, then reload. No changes were made."; }
        Controls();
    }
    private async Task Change(HubRegistrationRequest request)
    {
        busy = true; Controls(); status.Text = "Waiting for Windows approval and registration completion…";
        try {
            var result = await execute(request);
            if (closed) return;
            if (result != 0) { Invalidate("Registration failed or may be partial. Reload inventory and inspect ASCOM registration.log before another action."); return; }
            Reload();
        } catch { if (!closed) Invalidate("Registration was cancelled, failed or has unknown completion. Reload inventory before another action; no automatic retry was made."); }
        finally { busy = false; if (!closed) Controls(); }
    }
    public static void Show(string directory, HubSelectionStore store)
    {
        HubRegisteredOutput[] Inventory() {
            var result = new List<HubRegisteredOutput>();
            foreach (var view in new[] { RegistryView.Registry32, RegistryView.Registry64 }) {
                using var root = RegistryKey.OpenBaseKey(RegistryHive.LocalMachine, view);
                result.AddRange(HubAscomRegistration.RegisteredOutputs(root));
            }
            return result.ToArray();
        }
        new HubAscomManagerWindow(directory, store, Inventory, request => request.RunElevated(directory)).ShowDialog();
    }
}
