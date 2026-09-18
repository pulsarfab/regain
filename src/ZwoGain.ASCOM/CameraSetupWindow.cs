using System.Globalization;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using ZwoGain.Rotator;

namespace ZwoGain.Ascom;

internal static class CameraSetupWindow
{
    internal static void Show(LocalCamera session, int slot)
    {
        if (Application.Current is not null) { Application.Current.Dispatcher.Invoke(() => Open(session, slot)); return; }
        Exception? error = null;
        var thread = new Thread(() => { try { Open(session, slot); } catch (Exception e) { error = e; } });
        thread.SetApartmentState(ApartmentState.STA); thread.Start(); thread.Join();
        if (error is not null) throw new ASCOM.DriverException(error.Message, error);
    }
    private static void Open(LocalCamera session, int slot)
    {
        var profile = JsonNode.Parse(session.Request("profile").GetRawText())!;
        bool owned = !session.Get<bool>("connected"), busy = false, loading = true;
        var root = new DockPanel { Margin = new Thickness(16) };
        var heading = new TextBlock { Text = $"Camera {slot + 1}", FontSize = 18, FontWeight = FontWeights.SemiBold, Margin = new Thickness(0, 0, 0, 12) };
        DockPanel.SetDock(heading, Dock.Top); root.Children.Add(heading);
        var footer = new StackPanel { Margin = new Thickness(0, 12, 0, 0) };
        DockPanel.SetDock(footer, Dock.Bottom); root.Children.Add(footer);
        var feedback = new TextBlock { TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 0, 0, 8) };
        footer.Children.Add(feedback);
        var bottom = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right }; footer.Children.Add(bottom);
        var tabs = new TabControl(); root.Children.Add(tabs);
        var window = new Window { Title = $"ZWOgain Camera {slot + 1} Setup", Width = 720, MinWidth = 580,
            Height = Math.Min(540, SystemParameters.WorkArea.Height * .9), MaxHeight = SystemParameters.WorkArea.Height * .9,
            Content = root, Owner = Application.Current?.MainWindow, WindowStartupLocation = WindowStartupLocation.CenterOwner };
        SetupTheme.Apply(window);
        var editors = new List<Control>(); var readers = new List<Action>();
        bool Connected() => session.Get<bool>("connected");
        void Update() { bool connected = Connected(); foreach (var c in editors) c.IsEnabled = !busy && !connected; }
        void Save() {
            if (loading || Connected()) return;
            foreach (var read in readers) read();
            session.Request("configure", parameters: profile);
            feedback.Text = "Settings saved.";
        }
        async Task Run(Func<Task> action) {
            if (busy) return; busy = true; Update(); feedback.Text = "";
            try { await action(); } catch (Exception e) { feedback.Text = e.GetBaseException().Message; }
            finally { busy = false; Update(); }
        }
        void TrySave() { try { Save(); } catch (Exception e) { feedback.Text = e.Message; } }
        StackPanel Tab(string name, string description) {
            var body = new StackPanel { Margin = new Thickness(12) };
            body.Children.Add(new TextBlock { Text = description, TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 0, 0, 18) });
            tabs.Items.Add(new TabItem { Header = name, Content = new ScrollViewer { Content = body, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled } }); return body;
        }
        Button Button(Panel parent, string label, Func<Task> action, bool offline = true) {
            var b = new Button { Content = label, MinWidth = 90, HorizontalAlignment = HorizontalAlignment.Left, Padding = new Thickness(12, 6, 12, 6), Margin = new Thickness(0, 0, 8, 10) };
            parent.Children.Add(b); if (offline) editors.Add(b); b.Click += async (_, _) => await Run(action); return b;
        }
        TextBox Field(Panel parent, string label, string value, Action<string> read) {
            var row = new Grid { Margin = new Thickness(0, 0, 0, 12) };
            row.ColumnDefinitions.Add(new ColumnDefinition()); row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(170) });
            row.Children.Add(new TextBlock { Text = label, TextWrapping = TextWrapping.Wrap, VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(0, 0, 16, 0) });
            var box = new TextBox { Text = value, MinHeight = 28, VerticalContentAlignment = VerticalAlignment.Center }; AutomationProperties.SetName(box, label);
            Grid.SetColumn(box, 1); row.Children.Add(box); parent.Children.Add(row); editors.Add(box);
            readers.Add(() => read(box.Text.Trim())); box.LostKeyboardFocus += (_, _) => TrySave(); return box;
        }
        double Number(string text, string label, double min, double max, bool integer = false) {
            if (!double.TryParse(text, NumberStyles.Float, CultureInfo.InvariantCulture, out var n) || double.IsNaN(n) || double.IsInfinity(n) || n < min || n > max || (integer && n != Math.Truncate(n)))
                throw new ArgumentException($"{label} must be {(integer ? "a whole number " : "")}between {min} and {max}.");
            return n;
        }
        var device = Tab("Device", "Choose a local camera. Settings save automatically; disconnect to change them.");
        var picker = new ComboBox { MinHeight = 30, Margin = new Thickness(0, 0, 0, 12), DisplayMemberPath = "Label" }; editors.Add(picker); device.Children.Add(picker); AutomationProperties.SetName(picker, "Camera");
        if (profile["camera"] is JsonNode saved) { picker.Items.Add(new Choice(saved.DeepClone())); picker.SelectedIndex = 0; }
        var backend = new ComboBox { ItemsSource = new[] { "ZWO SDK", "Direct USB" }, SelectedIndex = profile["direct"]!.GetValue<bool>() ? 1 : 0, MinHeight = 30, Margin = new Thickness(0, 0, 0, 12) };
        AutomationProperties.SetName(backend, "Camera backend"); device.Children.Add(new TextBlock { Text = "Camera backend" }); device.Children.Add(backend); editors.Add(backend);
        var fallback = new CheckBox { Content = "Allow SDK fallback for direct USB", IsChecked = profile["sdkFallback"]!.GetValue<bool>(), Margin = new Thickness(0, 0, 0, 12) }; device.Children.Add(fallback); editors.Add(fallback);
        var serial = Field(device, "Camera serial (optional)", profile["serial"]?.GetValue<string>() ?? "", v => profile["serial"] = v.Length == 0 ? null : JsonValue.Create(v));
        readers.Add(() => { profile["camera"] = (picker.SelectedItem as Choice)?.Camera.DeepClone(); profile["direct"] = backend.SelectedIndex == 1; profile["sdkFallback"] = backend.SelectedIndex == 1 && fallback.IsChecked == true; });
        picker.SelectionChanged += (_, _) => { if (loading || busy) return; serial.Text = ""; TrySave(); };
        backend.SelectionChanged += (_, _) => TrySave(); fallback.Click += (_, _) => TrySave();
        async Task Discover() {
            bool direct = backend.SelectedIndex == 1;
            var found = await Task.Run(() => session.Request("discover", parameters: new { direct }));
            var selected = picker.SelectedItem as Choice; loading = true;
            try {
                picker.Items.Clear();
                foreach (var item in found.EnumerateArray()) picker.Items.Add(new Choice(JsonNode.Parse(item.GetRawText())!));
                var match = picker.Items.Cast<Choice>().FirstOrDefault(c => c.Camera["name"]!.GetValue<string>() == selected?.Camera["name"]?.GetValue<string>());
                if (match is null && selected is not null) { picker.Items.Insert(0, selected); match = selected; }
                picker.SelectedItem = match ?? (picker.Items.Count == 1 ? picker.Items[0] : null);
            } finally { loading = false; }
            if (picker.SelectedItem is not null) Save();
            else feedback.Text = picker.Items.Count == 0 ? "No camera found. Check USB and close other camera controllers." : "Choose a camera above.";
        }
        Button(device, "Refresh", Discover);
        var recovery = Tab("Recovery", "Retry limits apply to this camera. Long exposures are replaced only within the configured duration limit.");
        var cooler = Tab("Cooler", "Recovery restores the prior cooler state and waits near its previous measured temperature.");
        var timeouts = Tab("Timeouts", "Maximum time allowed for camera commands, downloads and recovery.");
        void Recovery(Panel parent, string key, string label, double min, double max, bool integer = false) => Field(parent, label, profile["recovery"]![key]!.ToJsonString(), v => { double n = Number(v, label, min, max, integer); profile["recovery"]![key] = integer ? JsonValue.Create((int)n) : JsonValue.Create(n); });
        Recovery(recovery, "maxRetries", "Replacement exposures", 0, 20, true);
        Recovery(recovery, "maximumRetryExposureSeconds", "Maximum exposure to replace (s)", 0, 86400);
        Recovery(recovery, "readyFrameDownloadRetries", "SDK read retries", 0, 5, true);
        Recovery(recovery, "directReadRetries", "Direct read retries", 0, 5, true);
        Recovery(recovery, "reconnectDelaySeconds", "Reconnect delay (s)", .001, 3600);
        Recovery(cooler, "coolingTimeoutSeconds", "Cooling timeout (s)", .001, 3600);
        Recovery(cooler, "temperatureToleranceC", "Temperature tolerance (°C)", .001, 3600);
        Recovery(cooler, "coolingStableSamples", "Stable samples", 1, 60, true);
        Recovery(cooler, "coolingSampleSeconds", "Sample interval (s)", .001, 3600);
        Recovery(timeouts, "commandTimeoutSeconds", "Command timeout (s)", .001, 3600);
        Recovery(timeouts, "downloadTimeoutSeconds", "Download timeout (s)", .001, 3600);
        Recovery(timeouts, "exposureGraceSeconds", "Exposure grace (s)", .001, 3600);
        var controls = Tab("Controls", "Optional connection defaults. Leave a value blank to keep the camera setting.");
        void Control(Panel parent, string key, string label) => Field(parent, label, profile["controls"]![key]?.ToJsonString() ?? "", v => {
            if (v == "") profile["controls"]!.AsObject().Remove(key);
            else profile["controls"]![key] = (long)Number(v, label, int.MinValue, int.MaxValue, true);
        });
        Control(controls, "0", "Gain"); Control(controls, "5", "Offset"); Control(controls, "6", "USB limit"); Control(controls, "21", "Dew heater (0–1)"); Control(controls, "22", "Fan speed"); Control(controls, "23", "Power LED brightness");
        Control(cooler, "16", "Target temperature (°C)"); Control(cooler, "17", "Cooler enabled (0–1)");
        var connection = Button(bottom, Connected() ? "Connected" : "Connect", async () => {
            if (!owned) { feedback.Text = "Disconnect through the application's equipment pane."; return; }
            bool connected = Connected(); if (!connected) Save();
            await Task.Run(() => session.Put("connected", !connected));
        }, false);
        var close = new Button { Content = "Close", MinWidth = 90, Padding = new Thickness(12, 6, 12, 6), Margin = new Thickness(0, 0, 0, 10) }; bottom.Children.Add(close); close.Click += (_, _) => window.Close();
        var timer = new System.Windows.Threading.DispatcherTimer { Interval = TimeSpan.FromSeconds(1) };
        timer.Tick += (_, _) => { if (!busy) { try { connection.Content = Connected() ? "Disconnect" : "Connect"; Update(); } catch (Exception e) { feedback.Text = e.Message; } } };
        loading = false; Update();
        window.Loaded += async (_, _) => { timer.Start(); if (!Connected()) await Run(Discover); };
        window.Closing += (_, e) => { if (busy) { e.Cancel = true; return; } try { Save(); } catch (Exception ex) { feedback.Text = ex.Message; e.Cancel = true; } };
        try { window.ShowDialog(); } finally { timer.Stop(); if (owned) session.Put("connected", false); }
    }
    private sealed class Choice(JsonNode camera) { public JsonNode Camera { get; } = camera; public string Label => Camera["name"]!.GetValue<string>(); }
}
