using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using Microsoft.Win32;
using Regain.Rotator;

namespace Regain.Hub;

/// Shared native output selector. It acquires no equipment lease and starts no
/// HTTP publisher; the same saved output identities serve NINA and ASCOM setup.
public sealed class HubSelectionWindow : Window
{
    private readonly string executable, type;
    private readonly HubSelectionStore store;
    private Guid expectedRevision;
    private bool selectionsReadable;
    private readonly CancellationTokenSource lifetime = new();
    private readonly TextBox path = new() { MinWidth = 450 };
    private readonly ComboBox outputs = new() { MinWidth = 450, DisplayMemberPath = nameof(Choice.Label) };
    private readonly TextBlock status = new() { TextWrapping = TextWrapping.Wrap };
    private readonly Button load = new() { Content = "Load hub outputs", Padding = new Thickness(14, 8, 14, 8) };
    private readonly Button save = new() { Content = "Save selected output", Padding = new Thickness(14, 8, 14, 8), IsEnabled = false };
    private readonly Button edit = new() { Content = "Edit shared configuration", Padding = new Thickness(14, 8, 14, 8), IsEnabled = false };
    private HubSelection? result;
    private string? loadedPath;
    private Guid loadedInstance;
    private bool busy;
    private bool closed;
    private sealed class Choice(HubSelection binding)
    {
        internal HubSelection Binding { get; } = binding;
        public string Label => Binding.Label + (Binding.Simulated ? " [SIMULATION]" : "") + " — " + Binding.OutputId.ToString("D");
    }
    private HubSelectionWindow(string executable, HubSelectionStore store, string type, HubSelection? current)
    {
        this.executable = executable; this.store = store; this.type = type;
        try { expectedRevision = store.Load().Revision; selectionsReadable = true; }
        catch { selectionsReadable = false; }
        Title = "PulsarFab regain — hub " + type; Width = 760; Height = 520; MinWidth = 620;
        WindowStartupLocation = WindowStartupLocation.CenterScreen; SetupTheme.Apply(this);
        var panel = new StackPanel { Margin = new Thickness(24) };
        Content = new ScrollViewer { Content = panel, VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        panel.Children.Add(new TextBlock { Text = "Choose a shared hub output", FontSize = 24, Margin = new Thickness(0, 0, 0, 16) });
        panel.Children.Add(new TextBlock { Text = "Select a saved hub configuration, then load its outputs. This starts or attaches to the shared local host. The selected device connects when NINA or ASCOM connects it.", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 0, 0, 16) });
        panel.Children.Add(new TextBlock { Text = "Hub configuration file" });
        panel.Children.Add(path);
        var browse = new Button { Content = "Browse…", Padding = new Thickness(14, 8, 14, 8), HorizontalAlignment = HorizontalAlignment.Left };
        browse.Click += (_, _) => {
            var dialog = new OpenFileDialog { Filter = "Hub configuration (*.json)|*.json", CheckFileExists = true };
            if (dialog.ShowDialog(this) == true) path.Text = dialog.FileName;
        };
        panel.Children.Add(browse); panel.Children.Add(load); panel.Children.Add(edit);
        panel.Children.Add(new TextBlock { Text = "Output (stable UUID)", Margin = new Thickness(0, 16, 0, 0) });
        panel.Children.Add(outputs); panel.Children.Add(save);
        var manage = new Button { Content = "Manage saved output choices", Padding = new Thickness(14, 8, 14, 8), HorizontalAlignment = HorizontalAlignment.Left };
        panel.Children.Add(manage);
        manage.Click += (_, _) => {
            if (busy) return;
            HubSelectionManagerWindow.Show(this, store, type);
            // Management is explicit reconciliation of this same saved file.
            // A race after this reload is still caught by Save's revision guard.
            try {
                expectedRevision = store.Load().Revision; selectionsReadable = true;
                save.IsEnabled = loadedPath == path.Text && outputs.SelectedItem is Choice;
            } catch { selectionsReadable = false; save.IsEnabled = false; status.Text = SelectionError; }
        };
        panel.Children.Add(status);
        if (!selectionsReadable) status.Text = SelectionError;
        path.Text = current?.ConfigPath ?? "";
        path.TextChanged += (_, _) => { save.IsEnabled = false; edit.IsEnabled = false; outputs.ItemsSource = null; loadedPath = null; };
        outputs.SelectionChanged += (_, _) => save.IsEnabled = selectionsReadable && !busy && loadedPath == path.Text && outputs.SelectedItem is Choice;
        load.Click += async (_, _) => {
            if (busy) return;
            busy = true; load.IsEnabled = false; save.IsEnabled = false; edit.IsEnabled = false; path.IsEnabled = false; browse.IsEnabled = false;
            outputs.ItemsSource = null; loadedPath = null; status.Text = "Attaching to the shared hub…";
            try {
                var selectedPath = path.Text;
                var attachment = await HubAttachment.AttachAsync(executable, selectedPath, cancellation: lifetime.Token);
                using var client = await HubClient.ConnectAsync(attachment, cancellation: lifetime.Token);
                var catalog = await client.RequestAsync(JsonSerializer.SerializeToElement(new { op = "listDevices" }), lifetime.Token);
                var choices = catalog.EnumerateArray().Where(d => d.GetProperty("deviceType").GetString() == type).Select(d => new Choice(new HubSelection {
                    ConfigPath = selectedPath, InstanceId = attachment.InstanceId, OutputId = d.GetProperty("id").GetGuid(),
                    DeviceType = type, Label = d.GetProperty("label").GetString()!, Simulated = d.GetProperty("simulated").GetBoolean()
                })).ToArray();
                lifetime.Token.ThrowIfCancellationRequested();
                outputs.ItemsSource = choices; loadedPath = selectedPath; loadedInstance = attachment.InstanceId;
                outputs.SelectedItem = choices.FirstOrDefault(c => current?.InstanceId == c.Binding.InstanceId && current.OutputId == c.Binding.OutputId)
                    ?? choices.FirstOrDefault();
                status.Text = !selectionsReadable ? SelectionError : choices.Length == 0 ? "No outputs of this class exist in this hub configuration." :
                    "Only output identities are saved here. Source mappings, permissions and safety policy belong to the shared hub configuration.";
            } catch (OperationCanceledException) { if (!lifetime.IsCancellationRequested) status.Text = "Hub attachment timed out. Check the host and configuration before trying again."; }
            catch { status.Text = "Could not load hub outputs. Check the configuration file and Regain host installation."; }
            finally {
                busy = false; load.IsEnabled = true; path.IsEnabled = true; browse.IsEnabled = true;
                save.IsEnabled = selectionsReadable && !closed && loadedPath == path.Text && outputs.SelectedItem is Choice;
                edit.IsEnabled = !closed && loadedPath == path.Text;
                if (closed) lifetime.Dispose();
            }
        };
        edit.Click += (_, _) => {
            if (busy || loadedPath is null || loadedPath != path.Text) return;
            try { HubConfigurationWindow.Show(this, executable, loadedPath, loadedInstance);
                status.Text = "Load hub outputs again to refresh saved configuration changes."; }
            catch { status.Text = "Could not open shared configuration setup."; }
            finally { loadedPath = null; outputs.ItemsSource = null; save.IsEnabled = false; edit.IsEnabled = false; }
        };
        save.Click += (_, _) => {
            if (!selectionsReadable || busy || loadedPath != path.Text || outputs.SelectedItem is not Choice choice) return;
            try { store.Save(choice.Binding, expectedRevision); result = choice.Binding.Copy(); DialogResult = true; }
            catch { status.Text = "Could not save: another editor may have changed the selections. Close and reopen setup to reload them."; }
        };
        // Cancellation closes only this helper/client. The shared host remains
        // alive; no candidate PID or equipment worker is terminated by setup.
        Closed += (_, _) => { closed = true; lifetime.Cancel(); if (!busy) lifetime.Dispose(); };
    }
    private string SelectionError => "Saved frontend selections could not be read. Correct or move aside " + store.Path +
        " and reopen setup. This window will not overwrite an unreadable selection file.";
    public static HubSelection? Select(string executable, HubSelectionStore store, string type, HubSelection? current = null)
    {
        if (!HubSelection.Types.Contains(type, StringComparer.Ordinal)) throw new ArgumentException("Unsupported hub device class", nameof(type));
        var window = new HubSelectionWindow(executable, store, type, current);
        window.ShowDialog(); return window.result;
    }
}
