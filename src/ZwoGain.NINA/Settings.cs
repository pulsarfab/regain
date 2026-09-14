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
        var panel = new StackPanel { Margin = new Thickness(16) };
        panel.Children.Add(new TextBlock { Text = "Camera and recovery settings apply on the next connection.", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 0, 0, 12) });
        var remembered = Cameras.Load();
        // NINA's toggle template replaces CheckBox.Content with ON/OFF text.
        panel.Children.Add(new TextBlock { Text = "Try SDK-less driver (experimental; ASI676MC and Duo main/guide)", TextWrapping = TextWrapping.Wrap });
        var direct = new CheckBox { IsChecked = remembered?.UseDirectDriver == true, HorizontalAlignment = HorizontalAlignment.Left, Margin = new Thickness(0, 2, 0, 8) };
        panel.Children.Add(direct);
        panel.Children.Add(new TextBlock { Text = "Default: supervised ZWO SDK. Direct RAW16 supports ASI676MC bin 1, ASI2600MM Duo bins 1-4 with cooling/dew control, and ASI220MM Mini guide bins 1-2. Verified direct limits: main/676 30 seconds; guide 10 seconds. USB limit is fixed at 40.", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 0, 0, 12) });
        panel.Children.Add(new TextBlock { Text = "Allow SDK fallback from the experimental driver", TextWrapping = TextWrapping.Wrap });
        var fallback = new CheckBox { IsChecked = remembered?.AllowSdkFallback == true, IsEnabled = direct.IsChecked == true,
            HorizontalAlignment = HorizontalAlignment.Left, Margin = new Thickness(0, 2, 0, 8) };
        panel.Children.Add(fallback);
        panel.Children.Add(new TextBlock { Text = "Uses the same camera serial and restores controls. Unsupported capture settings switch before exposure; failures switch on the next permitted retry. The SDK remains active until disconnect. Retry duration and count limits still apply.", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 0, 0, 12) });
        panel.Children.Add(new TextBlock { Text = "Camera" });
        var picker = new ComboBox { DisplayMemberPath = nameof(CameraChoice.Label), MinWidth = 300, Margin = new Thickness(0, 2, 0, 8) };
        if (remembered is not null)
        {
            picker.Items.Add(new CameraChoice(remembered.Camera, remembered.Camera.Name + " (saved; availability not checked)"));
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
        var entries = new Dictionary<string, TextBox>();
        var current = Load();
        var labels = new Dictionary<string, string>
        {
            [nameof(RecoveryOptions.MaxRetries)] = "Recovery retries after the initial attempt",
            [nameof(RecoveryOptions.MaximumRetryExposureSeconds)] = "Maximum exposure eligible for retries (seconds; 0 disables retries)",
            [nameof(RecoveryOptions.ReconnectDelaySeconds)] = "USB reconnect delay (seconds)",
            [nameof(RecoveryOptions.CommandTimeoutSeconds)] = "Camera command timeout (seconds)",
            [nameof(RecoveryOptions.DownloadTimeoutSeconds)] = "Download watchdog (seconds)",
            [nameof(RecoveryOptions.ExposureGraceSeconds)] = "Exposure completion grace (seconds)",
            [nameof(RecoveryOptions.CoolingTimeoutSeconds)] = "Cooling recovery deadline (seconds)",
            [nameof(RecoveryOptions.TemperatureToleranceC)] = "Prior temperature tolerance (C)",
            [nameof(RecoveryOptions.CoolingStableSamples)] = "Consecutive cooling samples",
            [nameof(RecoveryOptions.CoolingSampleSeconds)] = "Cooling sample interval (seconds)",
            [nameof(RecoveryOptions.ReadyFrameDownloadRetries)] = "Experimental re-downloads while SDK reports success (default 0)",
            [nameof(RecoveryOptions.DirectReadRetries)] = "SDK-less retained-frame read retries (default 2; 0–5)"
        };
        foreach (var property in typeof(RecoveryOptions).GetProperties())
        {
            panel.Children.Add(new TextBlock { Text = labels[property.Name], TextWrapping = TextWrapping.Wrap });
            var field = new TextBox { Text = Convert.ToString(property.GetValue(current), System.Globalization.CultureInfo.InvariantCulture), Margin = new Thickness(0, 2, 0, 8) };
            entries[property.Name] = field;
            panel.Children.Add(field);
        }
        var status = new TextBlock { TextWrapping = TextWrapping.Wrap };
        panel.Children.Add(status);
        var button = new Button { Content = "Save", Padding = new Thickness(16, 6, 16, 6) };
        panel.Children.Add(button);
        var window = new Window
        {
            Title = "ZWOgain Retryable Camera Setup",
            Width = 510,
            Height = Math.Min(760, SystemParameters.WorkArea.Height * .9),
            MaxHeight = SystemParameters.WorkArea.Height * .9,
            Content = new ScrollViewer { Content = panel, VerticalScrollBarVisibility = ScrollBarVisibility.Auto },
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
                var choices = found.GroupBy(c => c.Name).Select(g => new CameraChoice(g.First(), g.Key + (g.Count() > 1 ? " (multiple attached; enter serial)" : ""))).ToList();
                if (chosen is not null && choices.All(c => c.Camera.Name != chosen.Camera.Name))
                    choices.Insert(0, chosen with { Label = chosen.Camera.Name + " (not currently detected)" });
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
                    throw new InvalidOperationException("Choose ASI676MC, ASI2600MM Duo or ASI220MM Mini for experimental capture, or select the SDK backend.");
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
