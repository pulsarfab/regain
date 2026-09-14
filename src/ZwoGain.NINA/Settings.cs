using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using ZwoGain.Core;

namespace ZwoGain.NINA;

internal static class Settings
{
    private static readonly string Folder = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "ZwoGain");
    private static readonly string FilePath = Path.Combine(Folder, "recovery.json");
    internal static readonly CameraSelectionStore Cameras = new(Path.Combine(Folder, "camera.json"));
    private sealed record CameraChoice(CameraDescriptor Camera, string Label);
    internal static string CameraLabel(string name) => name switch
    {
        "ZWO ASI2600MM Duo" or "ZWO ASI2600MM Pro" => "ASI2600MM Pro",
        "ZWO ASI220MM Mini" => "ASI220MM Mini (guide)",
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
        var entries = new Dictionary<string, TextBox>();
        var current = Load();
        void AddFields(StackPanel body, params (string Property, string Label)[] fields)
        {
            foreach (var (name, label) in fields)
            {
                var property = typeof(RecoveryOptions).GetProperty(name)!;
                var row = new Grid { Margin = new Thickness(0, 0, 0, 14) };
                row.ColumnDefinitions.Add(new ColumnDefinition());
                row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(110) });
                row.Children.Add(new TextBlock { Text = label, TextWrapping = TextWrapping.Wrap,
                    VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(0, 0, 16, 0) });
                var field = new TextBox { Text = Convert.ToString(property.GetValue(current), System.Globalization.CultureInfo.InvariantCulture),
                    VerticalContentAlignment = VerticalAlignment.Center, MinHeight = 28 };
                System.Windows.Automation.AutomationProperties.SetName(field, label);
                Grid.SetColumn(field, 1);
                row.Children.Add(field);
                entries[name] = field;
                body.Children.Add(row);
            }
        }
        var recovery = AddTab("Recovery", "Reconnect and repeat failed exposures within these limits.");
        AddFields(recovery,
            (nameof(RecoveryOptions.MaxRetries), "Maximum retries"),
            (nameof(RecoveryOptions.MaximumRetryExposureSeconds), "Exposure limit (s; 0 disables retries)"),
            (nameof(RecoveryOptions.ReconnectDelaySeconds), "Reconnect delay (s)"));
        var cooling = AddTab("Cooling", "Restore the setpoint; wait for the previous temperature and cooler output.");
        AddFields(cooling,
            (nameof(RecoveryOptions.TemperatureToleranceC), "Temperature tolerance (°C)"),
            (nameof(RecoveryOptions.CoolingStableSamples), "Stable readings"),
            (nameof(RecoveryOptions.CoolingSampleSeconds), "Sample interval (s)"),
            (nameof(RecoveryOptions.CoolingTimeoutSeconds), "Recovery timeout (s)"));
        entries[nameof(RecoveryOptions.TemperatureToleranceC)].ToolTip = "Further cooling toward the setpoint is accepted. Cooler output must reach at least its previous value minus 10 percentage points, if reported.";
        var advanced = AddTab("Advanced");
        AddFields(advanced,
            (nameof(RecoveryOptions.CommandTimeoutSeconds), "Command timeout (s)"),
            (nameof(RecoveryOptions.DownloadTimeoutSeconds), "Download timeout (s)"),
            (nameof(RecoveryOptions.ExposureGraceSeconds), "Exposure grace period (s)"),
            (nameof(RecoveryOptions.ReadyFrameDownloadRetries), "SDK re-download retries (experimental)"),
            (nameof(RecoveryOptions.DirectReadRetries), "Direct read retries (0-5)"));
        entries[nameof(RecoveryOptions.ReadyFrameDownloadRetries)].ToolTip = "Default: 0. Requires a ready frame in the SDK. The exposure limit applies.";
        entries[nameof(RecoveryOptions.DirectReadRetries)].ToolTip = "Default: 2. Guide retries read a new streaming frame. The exposure limit applies.";
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
            Title = "ZWOgain Camera Setup",
            Width = 650,
            Height = Math.Min(650, SystemParameters.WorkArea.Height * .9),
            MaxHeight = SystemParameters.WorkArea.Height * .9,
            Content = root,
            Owner = Application.Current?.MainWindow,
            WindowStartupLocation = WindowStartupLocation.CenterOwner
        };
        window.SetResourceReference(Window.BackgroundProperty, "BackgroundBrush");
        window.SetResourceReference(Window.ForegroundProperty, "PrimaryBrush");
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
                var values = entries.ToDictionary(k => k.Key, k => double.Parse(k.Value.Text, System.Globalization.CultureInfo.InvariantCulture));
                var options = JsonSerializer.Deserialize<RecoveryOptions>(JsonSerializer.Serialize(values))!;
                options.Validate();
                if (picker.SelectedItem is not CameraChoice choice)
                    throw new InvalidOperationException("Choose a camera before saving.");
                if (direct.IsChecked == true && choice.Camera.Name is not ("ZWO ASI676MC" or "ZWO ASI2600MM Duo" or "ZWO ASI220MM Mini"))
                    throw new InvalidOperationException("Direct capture is unavailable for this camera. Turn off Direct USB driver to use the SDK.");
                string? selectedSerial = string.IsNullOrWhiteSpace(serial.Text) ? null : serial.Text.Trim().ToLowerInvariant();
                if (selectedSerial is not null && (selectedSerial.Length != 16 || selectedSerial.Any(c => !Uri.IsHexDigit(c))))
                    throw new InvalidOperationException("The SDK serial number must contain 16 hexadecimal characters, or be left blank.");
                Directory.CreateDirectory(Folder);
                string temp = FilePath + "." + Guid.NewGuid().ToString("N") + ".tmp";
                File.WriteAllText(temp, JsonSerializer.Serialize(options, new JsonSerializerOptions { WriteIndented = true }));
                File.Move(temp, FilePath, true);
                Cameras.Save(new CameraSelection(choice.Camera, selectedSerial, direct.IsChecked == true, fallback.IsChecked == true));
                window.Close();
            }
            catch (Exception e) { status.Text = e.Message; }
        };
        window.ShowDialog();
    }
}
