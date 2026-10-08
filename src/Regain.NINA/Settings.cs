using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using Regain.Core;

namespace Regain.NINA;

internal static class Settings
{
    private static readonly string Folder = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Regain");
    private static readonly string FilePath = Regain.Rotator.RegainPaths.Profile("recovery.json");
    internal static readonly CameraSelectionStore Cameras = new(Regain.Rotator.RegainPaths.Profile("camera.json"));
    private sealed record CameraChoice(CameraDescriptor Camera, string Label);
    internal static string CameraLabel(string name) => name switch
    {
        "ZWO ASI2600MM Duo" or "ZWO ASI2600MM Pro" => "ASI2600MM Pro",
        "ZWO ASI220MM Mini" => "ASI220MM Mini (guide)",
        "ZWO ASI6200MM Pro" => "ASI6200MM Pro",
        _ => name
    };

    public static RecoveryOptions Load()
    {
        var options = File.Exists(FilePath) ? JsonSerializer.Deserialize<RecoveryOptions>(File.ReadAllText(FilePath)) ?? new() : new RecoveryOptions();
        options.Validate();
        return options;
    }
    public static void Show()
    {
        var dispatcher = Application.Current?.Dispatcher;
        if (dispatcher is not null && !dispatcher.CheckAccess())
        {
            dispatcher.Invoke(Show);
            return;
        }
        var root = new DockPanel { Margin = new Thickness(16) };
        var heading = new TextBlock { Text = "Reconnect to apply changes.",
            TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 0, 0, 12) };
        DockPanel.SetDock(heading, Dock.Top);
        root.Children.Add(heading);
        var footer = new StackPanel { Margin = new Thickness(0, 12, 0, 0) };
        DockPanel.SetDock(footer, Dock.Bottom);
        root.Children.Add(footer);
        var tabs = new TabControl();
        root.Children.Add(tabs);
        StackPanel AddTab(string title, string? description = null)
        {
            var body = new StackPanel { Margin = new Thickness(12) };
            if (description is not null)
                body.Children.Add(new TextBlock { Text = description, TextWrapping = TextWrapping.Wrap,
                    Margin = new Thickness(0, 0, 0, 16) });
            tabs.Items.Add(new TabItem { Header = title, Content = new ScrollViewer { Content = body,
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled } });
            return body;
        }
        var panel = AddTab("Camera");
        var remembered = Cameras.Load();
        panel.Children.Add(new TextBlock { Text = "Camera" });
        var picker = new ComboBox { DisplayMemberPath = nameof(CameraChoice.Label), MinWidth = 300, Margin = new Thickness(0, 2, 0, 8) };
        if (remembered is not null)
        {
            picker.Items.Add(new CameraChoice(remembered.Camera, CameraLabel(remembered.Camera.Name) + " (saved)"));
            picker.SelectedIndex = 0;
        }
        panel.Children.Add(picker);
        var refresh = new Button { Content = "Refresh cameras", HorizontalAlignment = HorizontalAlignment.Left, Padding = new Thickness(8, 4, 8, 4) };
        panel.Children.Add(refresh);
        panel.Children.Add(new TextBlock { Text = "Serial number (optional)", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 8, 0, 0) });
        var serial = new TextBox { Text = remembered?.Serial ?? "", Margin = new Thickness(0, 2, 0, 8),
            ToolTip = "Saved after connecting. Enter a serial to select between cameras of the same model." };
        panel.Children.Add(serial);
        var cameraStatus = new TextBlock { TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 0, 0, 12) };
        panel.Children.Add(cameraStatus);
        string? selectedName = remembered?.Camera.Name;
        picker.SelectionChanged += (_, _) =>
        {
            if (picker.SelectedItem is not CameraChoice choice || choice.Camera.Name == selectedName) return;
            selectedName = choice.Camera.Name;
            serial.Text = choice.Camera.Name == remembered?.Camera.Name ? remembered.Serial ?? "" : "";
        };
        // NINA's toggle template replaces CheckBox.Content with ON/OFF text.
        panel.Children.Add(new TextBlock { Text = "Direct USB driver (experimental)", TextWrapping = TextWrapping.Wrap });
        var direct = new CheckBox { IsChecked = remembered?.UseDirectDriver == true, HorizontalAlignment = HorizontalAlignment.Left, Margin = new Thickness(0, 2, 0, 8) };
        panel.Children.Add(direct);
        panel.Children.Add(new TextBlock { Text = "Off: use the ZWO SDK.", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 0, 0, 12) });
        panel.Children.Add(new TextBlock { Text = "Fall back to SDK", TextWrapping = TextWrapping.Wrap });
        var fallback = new CheckBox { IsChecked = remembered?.AllowSdkFallback == true, IsEnabled = direct.IsChecked == true,
            HorizontalAlignment = HorizontalAlignment.Left, Margin = new Thickness(0, 2, 0, 8) };
        panel.Children.Add(fallback);
        fallback.ToolTip = "If direct capture fails or is unsupported, use the SDK until disconnect. Retry limits still apply.";
        var current = Load();
        var recovery = AddTab("Recovery", "Replacement exposures and same-frame rereads use separate limits.");
        var cooling = AddTab("Cooling", "Recovery restores the prior cooler state before a replacement exposure.");
        var advanced = AddTab("Advanced");
        advanced.Children.Add(new TextBlock { Text = "Available fan and LED controls depend on the selected camera.",
            TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 0, 0, 10) });
        TextBox OptionalCameraControl(string label, int? value) {
            advanced.Children.Add(new TextBlock {Text=label, Margin=new Thickness(0,0,0,2)});
            var field = new TextBox {Text=value?.ToString() ?? "", MinHeight=28, Margin=new Thickness(0,0,0,10),
                ToolTip="0–255. Leave blank to use the camera's current value. Applied when connecting."};
            System.Windows.Automation.AutomationProperties.SetName(field,label);
            advanced.Children.Add(field);
            return field;
        }
        var fanSpeed = OptionalCameraControl("Fan speed",remembered?.FanSpeed);
        var ledBrightness = OptionalCameraControl("Power LED brightness",remembered?.PowerLedBrightness);
        void UpdateAuxiliaryControls() {
            bool supported = (picker.SelectedItem as CameraChoice)?.Camera.Name is "ZWO ASI6200MM Pro" or "ZWO ASI2600MM Pro";
            fanSpeed.IsEnabled = ledBrightness.IsEnabled = supported;
        }
        picker.SelectionChanged += (_,_) => UpdateAuxiliaryControls();
        UpdateAuxiliaryControls();
        var recoveryJson = new JsonSerializerOptions { PropertyNamingPolicy = JsonNamingPolicy.CamelCase };
        var recoveryForm = new Regain.Rotator.CameraRecoveryForm(
            JsonSerializer.SerializeToElement(current, recoveryJson),
            section => section == "Cooling" ? cooling : section == "Timeouts" ? advanced : recovery);
        var status = new TextBlock { TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 0, 0, 8) };
        footer.Children.Add(status);
        var actions = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right };
        var cancelButton = new Button { Content = "Cancel", IsCancel = true, MinWidth = 88, Padding = new Thickness(16, 6, 16, 6), Margin = new Thickness(0, 0, 8, 0) };
        var button = new Button { Content = "Save", MinWidth = 88, Padding = new Thickness(16, 6, 16, 6) };
        actions.Children.Add(cancelButton);
        actions.Children.Add(button);
        footer.Children.Add(actions);
        var window = new Window
        {
            Title = "PulsarFab regain Camera Setup",
            Width = 650,
            Height = Math.Min(650, SystemParameters.WorkArea.Height * .9),
            MaxHeight = SystemParameters.WorkArea.Height * .9,
            Content = root,
            Owner = Application.Current?.MainWindow,
            WindowStartupLocation = WindowStartupLocation.CenterOwner
        };
        Regain.Rotator.SetupTheme.Apply(window);
        using var discoveryCancel = new CancellationTokenSource();
        window.Closed += (_, _) => discoveryCancel.Cancel();
        async Task RefreshCameras()
        {
            refresh.IsEnabled = false;
            picker.IsEnabled = false;
            direct.IsEnabled = false;
            bool useDirect = direct.IsChecked == true;
            cameraStatus.Text = "Looking for ZWO cameras...";
            try
            {
                // Discovery only queries properties; it never opens another driver's camera.
                var found = await Task.Run(() => CameraProvider.DiscoverAsync(discoveryCancel.Token, useDirect));
                if (discoveryCancel.IsCancellationRequested) return;
                var chosen = picker.SelectedItem as CameraChoice;
                var choices = found.GroupBy(c => c.Name).Select(g => new CameraChoice(g.First(), CameraLabel(g.Key) + (g.Count() > 1 ? " (multiple attached; enter serial)" : ""))).ToList();
                if (chosen is not null && choices.All(c => c.Camera.Name != chosen.Camera.Name))
                    choices.Insert(0, chosen with { Label = CameraLabel(chosen.Camera.Name) + " (not currently detected)" });
                picker.Items.Clear();
                foreach (var choice in choices) picker.Items.Add(choice);
                picker.SelectedItem = choices.FirstOrDefault(c => c.Camera.Name == chosen?.Camera.Name);
                cameraStatus.Text = found.Count == 0 ? "No cameras detected. Connect a camera and refresh." : "";
            }
            catch (OperationCanceledException) { }
            catch (Exception e) { cameraStatus.Text = "Camera discovery failed: " + e.Message; }
            finally { refresh.IsEnabled = true; picker.IsEnabled = true; direct.IsEnabled = true; }
        }
        direct.Checked += async (_, _) => { fallback.IsEnabled = true; if (window.IsLoaded) await RefreshCameras(); };
        direct.Unchecked += async (_, _) => { fallback.IsEnabled = false; if (window.IsLoaded) await RefreshCameras(); };
        refresh.Click += async (_, _) => await RefreshCameras();
        window.Loaded += async (_, _) => await RefreshCameras();
        button.Click += (_, _) =>
        {
            try
            {
                var options = JsonSerializer.Deserialize<RecoveryOptions>(recoveryForm.Read().GetRawText(), recoveryJson)!;
                options.Validate();
                if (picker.SelectedItem is not CameraChoice choice)
                    throw new InvalidOperationException("Choose a camera before saving.");
                if (direct.IsChecked == true && choice.Camera.Name is not ("ZWO ASI585MM Pro" or "ZWO ASI676MC" or "ZWO ASI662MC" or "ZWO ASI2600MM Duo" or "ZWO ASI2600MM Pro" or "ZWO ASI220MM Mini" or "ZWO ASI6200MM Pro"))
                    throw new InvalidOperationException("Direct capture is unavailable for this camera. Turn off Direct USB driver to use the SDK.");
                string? selectedSerial = string.IsNullOrWhiteSpace(serial.Text) ? null : serial.Text.Trim().ToLowerInvariant();
                if (selectedSerial is not null && (selectedSerial.Length != 16 || selectedSerial.Any(c => !Uri.IsHexDigit(c))))
                    throw new InvalidOperationException("The SDK serial number must contain 16 hexadecimal characters, or be left blank.");
                static int? OptionalByte(TextBox field) {
                    if (!field.IsEnabled || string.IsNullOrWhiteSpace(field.Text)) return null;
                    if (!int.TryParse(field.Text,out int value) || value is <0 or >255)
                        throw new InvalidOperationException("Fan speed and LED brightness must be 0–255, or blank.");
                    return value;
                }
                int? fan = OptionalByte(fanSpeed), led = OptionalByte(ledBrightness);
                Directory.CreateDirectory(Folder);
                string temp = FilePath + "." + Guid.NewGuid().ToString("N") + ".tmp";
                File.WriteAllText(temp, JsonSerializer.Serialize(options, new JsonSerializerOptions { WriteIndented = true }));
                File.Move(temp, FilePath, true);
                Cameras.Save(new CameraSelection(choice.Camera, selectedSerial, direct.IsChecked == true, fallback.IsChecked == true,
                    fan,led));
                window.Close();
            }
            catch (Exception e) { status.Text = e.Message; }
        };
        window.ShowDialog();
    }
}
