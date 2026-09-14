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
    private static string CameraLabel(string name) => name switch
    {
        "ZWO ASI2600MM Duo" => "ASI2600MM Pro Duo - main camera",
        "ZWO ASI220MM Mini" => "ASI220MM Mini - guide camera (including Duo guide)",
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
        var heading = new TextBlock { Text = "Choose a camera and tune automatic recovery. Changes apply on the next connection.",
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
        panel.Children.Add(new TextBlock { Text = "Serial number (optional; remembered automatically after connecting)", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 8, 0, 0) });
        var serial = new TextBox { Text = remembered?.Serial ?? "", Margin = new Thickness(0, 2, 0, 8) };
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
        panel.Children.Add(new TextBlock { Text = "Use experimental SDK-less driver", TextWrapping = TextWrapping.Wrap });
        var direct = new CheckBox { IsChecked = remembered?.UseDirectDriver == true, HorizontalAlignment = HorizontalAlignment.Left, Margin = new Thickness(0, 2, 0, 8) };
        panel.Children.Add(direct);
        panel.Children.Add(new TextBlock { Text = "The ZWO SDK is the default. The direct option supports ASI676MC, ASI2600MM Pro Duo main and ASI220MM Mini guide. Other ASI2600MM Pro models use the SDK. See Advanced for direct capture limits.", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 0, 0, 12) });
        panel.Children.Add(new TextBlock { Text = "Allow SDK fallback from the experimental driver", TextWrapping = TextWrapping.Wrap });
        var fallback = new CheckBox { IsChecked = remembered?.AllowSdkFallback == true, IsEnabled = direct.IsChecked == true,
            HorizontalAlignment = HorizontalAlignment.Left, Margin = new Thickness(0, 2, 0, 8) };
        panel.Children.Add(fallback);
        panel.Children.Add(new TextBlock { Text = "Switch to the SDK when needed, keeping the same camera and settings. Failed exposures still obey the Recovery limits. The SDK then stays active until disconnect.", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 0, 0, 12) });
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
        var recovery = AddTab("Recovery", "A failed short exposure becomes a longer command: reconnect, restore settings, then try the same exposure again. Exposures above the cutoff report their first failure.");
        AddFields(recovery,
            (nameof(RecoveryOptions.MaxRetries), "Retries after the initial attempt"),
            (nameof(RecoveryOptions.MaximumRetryExposureSeconds), "Retry exposures up to (seconds; 0 disables retries)"),
            (nameof(RecoveryOptions.ReconnectDelaySeconds), "USB reconnect delay (seconds)"));
        var cooling = AddTab("Cooling", "Recovery restores the cooler target, then waits near the prior temperature. When power telemetry is available, output must also recover to within 10 percentage points below its prior level. This avoids resuming while a cold sensor is beginning to warm. Normal cooling controls are in NINA's Equipment panel.");
        AddFields(cooling,
            (nameof(RecoveryOptions.TemperatureToleranceC), "Tolerance around the prior temperature (C)"),
            (nameof(RecoveryOptions.CoolingStableSamples), "Consecutive readings within tolerance"),
            (nameof(RecoveryOptions.CoolingSampleSeconds), "Time between readings (seconds)"),
            (nameof(RecoveryOptions.CoolingTimeoutSeconds), "Maximum wait per recovery (seconds)"));
        var advanced = AddTab("Advanced", "Direct RAW16: ASI676MC bin 1; ASI2600MM Pro Duo main bins 1-4 with cooling/dew; ASI220MM Mini guide bins 1-2. Verified direct exposures: main/ASI676 up to 30 s, guide up to 10 s. Direct USB bandwidth is fixed at 40.");
        AddFields(advanced,
            (nameof(RecoveryOptions.CommandTimeoutSeconds), "Camera command timeout (seconds)"),
            (nameof(RecoveryOptions.DownloadTimeoutSeconds), "Download watchdog (seconds)"),
            (nameof(RecoveryOptions.ExposureGraceSeconds), "Exposure completion grace (seconds)"),
            (nameof(RecoveryOptions.ReadyFrameDownloadRetries), "SDK re-download attempts (experimental; default 0)"),
            (nameof(RecoveryOptions.DirectReadRetries), "Direct frame-read retries (0-5; default 2)"));
        advanced.Children.Add(new TextBlock { Text = "SDK re-downloads require the SDK to still report a ready frame. Direct guide retries resynchronize to a new streaming frame; they do not replay a retained image. The exposure cutoff also limits transfer retries.", TextWrapping = TextWrapping.Wrap });
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
            Title = "ZWOgain Retryable Camera Setup",
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
                cameraStatus.Text = found.Count == 0 ? "No cameras detected. A saved selection is retained; attach the camera and refresh." : "Choose a camera and save. For multiple cameras of the same model, enter its SDK serial number; otherwise connect each once with only that model attached.";
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
                    throw new InvalidOperationException("Choose ASI676MC, ASI2600MM Pro Duo main or ASI220MM Mini guide for experimental capture, or select the SDK backend.");
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
