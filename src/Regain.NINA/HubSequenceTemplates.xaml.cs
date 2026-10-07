using System.ComponentModel.Composition;
using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using Regain.Hub;
using Regain.Rotator;

namespace Regain.NINA;

[Export(typeof(ResourceDictionary))]
public partial class HubSequenceTemplates : ResourceDictionary
{
    public HubSequenceTemplates() { InitializeComponent(); }
    private async void ChooseGroup(object sender, RoutedEventArgs args)
    {
        if ((sender as FrameworkElement)?.DataContext is not MoveHubFocuserGroup step || step.Executing) return;
        await ChooseSavedGroup("focuser", () => step.Executing, async (path, instance) => {
            using var groups = await HubFocuserGroups.AttachAsync(HubEquipment.Executable, path, instance); return groups.Groups;
        }, (path, instance, group) => step.SelectGroup(path, instance, group.GetProperty("id").GetGuid(), group.GetProperty("label").GetString()!));
    }
    private static async Task ChooseSavedGroup(string kind, Func<bool> executing,
        Func<string, Guid, Task<IReadOnlyList<JsonElement>>> load, Action<string, Guid, JsonElement> select)
    {
        var picker = new Microsoft.Win32.OpenFileDialog { Filter = "Regain hub configuration (*.json)|*.json", CheckFileExists = true };
        if (picker.ShowDialog() != true) return;
        try {
            var instance = await ConfigurationInstance(picker.FileName);
            var groups = await load(picker.FileName, instance);
            if (executing()) return;
            var window = new Window { Title = $"PulsarFab regain — choose {kind} group", Width = 650, Height = 340, WindowStartupLocation = WindowStartupLocation.CenterScreen };
            SetupTheme.Apply(window);
            var panel = new StackPanel { Margin = new Thickness(20) }; window.Content = panel;
            panel.Children.Add(new TextBlock { Text = "Select a saved group. This loads configuration without connecting equipment. Group settings and policies are edited in hub setup.", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0,0,0,12) });
            var choices = new ComboBox();
            foreach (var group in groups) choices.Items.Add(new ComboBoxItem { Content = group.GetProperty("label").GetString(), Tag = group.Clone() });
            choices.SelectedIndex = choices.Items.Count == 0 ? -1 : 0; panel.Children.Add(choices);
            var save = new Button { Content = "Use selected group", IsEnabled = choices.Items.Count > 0, Padding = new Thickness(12,8,12,8), Margin = new Thickness(0,12,0,0) };
            save.Click += (_, _) => {
                if (executing()) { MessageBox.Show(window, "Wait for this sequence step to finish before changing its group.", $"Regain {kind} groups"); return; }
                if (choices.SelectedItem is ComboBoxItem item) { select(picker.FileName, instance, (JsonElement)item.Tag); window.DialogResult = true; }
            };
            panel.Children.Add(save); window.ShowDialog();
        } catch (Exception error) { MessageBox.Show(error is HubException ? "Could not load the saved groups from the shared hub. Open hub setup to inspect its configuration and host status." : error.Message, $"Regain {kind} groups"); }
    }
    private static async Task<Guid> ConfigurationInstance(string path)
    {
            // Bound actual reads as well as the length hint before attachment.
            using var file = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read | FileShare.Delete);
            if (file.Length > 4 * 1024 * 1024) throw new InvalidOperationException("The configuration is too large");
            var bytes = new byte[4 * 1024 * 1024 + 1]; var size = 0;
            while (size < bytes.Length) { var read = await file.ReadAsync(bytes.AsMemory(size)); if (read == 0) break; size += read; }
            if (size > 4 * 1024 * 1024) throw new InvalidOperationException("The configuration is too large");
            try { using var config = JsonDocument.Parse(bytes.AsMemory(0, size)); return config.RootElement.GetProperty("instanceId").GetGuid(); }
            finally { Array.Clear(bytes); }
    }
    private async void ChooseCameraGroup(object sender, RoutedEventArgs args)
    {
        if ((sender as FrameworkElement)?.DataContext is not CaptureHubCameraGroup step || step.Executing) return;
        await ChooseSavedGroup("camera", () => step.Executing, async (path, instance) => {
            using var groups = await HubCameraGroups.AttachAsync(HubEquipment.Executable, path, instance); return groups.Groups;
        }, step.SelectGroup);
    }
    private void InspectCameraGroup(object sender, RoutedEventArgs args)
    {
        if ((sender as FrameworkElement)?.DataContext is not CaptureHubCameraGroup step || step.Executing) return;
        HubConfigurationWindow.Show(null, HubEquipment.Executable, step.ConfigPath, step.InstanceId);
    }
    private void AllowNewCameraOperation(object sender, RoutedEventArgs args)
    {
        if ((sender as FrameworkElement)?.DataContext is not CaptureHubCameraGroup step || step.Executing) return;
        if (MessageBox.Show("Have you inspected every retained camera result and saved file? Allowing a new operation permits a new exposure on each camera. It does not resume the previous capture or save its images.", "Allow a new Regain camera capture", MessageBoxButton.YesNo, MessageBoxImage.Question) == MessageBoxResult.Yes)
            step.AllowNewOperationAfterInspection();
    }
    private void Inspect(object sender, RoutedEventArgs args)
    {
        if ((sender as FrameworkElement)?.DataContext is not MoveHubFocuserGroup step || step.Executing) return;
        HubConfigurationWindow.Show(null, HubEquipment.Executable, step.ConfigPath, step.InstanceId);
    }
    private void AllowNewOperation(object sender, RoutedEventArgs args)
    {
        if ((sender as FrameworkElement)?.DataContext is not MoveHubFocuserGroup step || step.Executing) return;
        if (MessageBox.Show("Have you inspected the retained group result and confirmed that each focuser is ready? Allowing a new operation permits this sequence step to send a new calibrated move. It does not resume or undo the previous operation.", "Allow a new Regain group operation", MessageBoxButton.YesNo, MessageBoxImage.Question) == MessageBoxResult.Yes)
            step.AllowNewOperationAfterInspection();
    }
}
