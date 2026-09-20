using System.Globalization;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Threading;

namespace ZwoGain.Rotator;

public sealed class AccessorySetupWindow : Window
{
    private readonly AccessorySession session;
    private readonly bool externallyOwned;
    private readonly ComboBox devices = new() { MinWidth = 350 };
    private readonly TextBlock summary = new() { TextWrapping = TextWrapping.Wrap };
    private readonly TextBlock motionStatus = new() { TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 12, 0, 8) };
    private readonly Button moveButton;
    private Button? calibrateButton;
    private readonly TextBlock message = new() { TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 12, 0, 0) };
    private readonly TextBox target = new() { Text = "0", Width = 160 };
    private readonly CheckBox beep = new() { Content = "Beep when movement starts" };
    private readonly CheckBox reverse = new() { Content = "Reverse motor direction" };
    private readonly TextBox backlash = new() { Width = 160 };
    private readonly TextBox speed = new() { Width = 160 };
    private readonly TextBox limit = new() { Width = 160 };
    private readonly StackPanel filters = new();
    private readonly List<(TextBox Name, TextBox Offset)> filterRows = [];
    private readonly DispatcherTimer timer = new() { Interval = TimeSpan.FromMilliseconds(500) };
    private bool busy;
    private bool initialized;

    public static void Show(AccessorySession session, Func<bool>? hasOtherClients = null)
    {
        var window = new AccessorySetupWindow(session, hasOtherClients);
        if (Application.Current?.MainWindow is Window owner && owner != window) window.Owner = owner;
        window.ShowDialog();
    }
    public AccessorySetupWindow(AccessorySession session, Func<bool>? hasOtherClients = null)
    {
        this.session = session; externallyOwned = session.Connected;
        Title = "ZWOgain " + (session.Kind == "fc3" ? "FocusCube3" : session.Kind.ToUpperInvariant()) + " Setup";
        Width = 730; Height = 650; MinWidth = 620; MinHeight = 540;
        WindowStartupLocation = WindowStartupLocation.CenterScreen;
        SetupTheme.Apply(this);
        var inputStyle = new Style(typeof(TextBox), TryFindResource(typeof(TextBox)) as Style);
        inputStyle.Setters.Add(new Setter(Control.PaddingProperty, new Thickness(8, 6, 8, 6)));
        inputStyle.Setters.Add(new Setter(FrameworkElement.MarginProperty, new Thickness(0, 0, 8, 6)));
        inputStyle.Setters.Add(new Setter(FrameworkElement.HorizontalAlignmentProperty, HorizontalAlignment.Left));
        Resources[typeof(TextBox)] = inputStyle;
        var checkStyle = new Style(typeof(CheckBox), TryFindResource(typeof(CheckBox)) as Style);
        checkStyle.Setters.Add(new Setter(FrameworkElement.MarginProperty, new Thickness(0, 4, 0, 8)));
        Resources[typeof(CheckBox)] = checkStyle;
        var root = new DockPanel { Margin = new Thickness(24) }; Content = root;
        var heading = new TextBlock { Text = session.Kind == "efw" ? "Electronic Filter Wheel" : session.Kind == "fc3" ? "Pegasus Astro FocusCube3" : "Electronic Automatic Focuser", FontSize = 25, Margin = new Thickness(0, 0, 0, 8) };
        DockPanel.SetDock(heading, Dock.Top); root.Children.Add(heading);
        var subheading = new TextBlock { Text = (session.Kind == "fc3" ? "ZWOgain  •  Native USB serial" : "ZWOgain  •  Native USB HID") + (Environment.GetEnvironmentVariable("ZWOGAIN_ACCESSORY_SIMULATE") == "1" ? "  •  Simulation" : ""), Margin = new Thickness(0, 0, 0, 20) };
        DockPanel.SetDock(subheading, Dock.Top); root.Children.Add(subheading);
        var footer = new StackPanel(); DockPanel.SetDock(footer, Dock.Bottom); root.Children.Add(footer);
        footer.Children.Add(message);
        footer.Children.Add(Button("Close", () => Close()));
        var tabs = new TabControl(); root.Children.Add(tabs);
        var device = Page(tabs, "Device");
        device.Children.Add(Label("Choose a USB device")); device.Children.Add(devices);
        device.Children.Add(Row(Button("Refresh", Discover), Button("Connect", () => {
            if (!session.Connected && devices.SelectedItem is CaaChoice choice) session.Select(choice.Serial);
            session.Connect(); initialized = false; RefreshStatus();
        }), Button("Disconnect", () => { if (externallyOwned || hasOtherClients?.Invoke() == true) throw new InvalidOperationException("Disconnect through the connected application"); session.Disconnect(); summary.Text = "Disconnected"; })));
        if (externallyOwned) {
            devices.Items.Add(new CaaChoice { Serial = session.Profile.Serial, Label = session.Kind.ToUpperInvariant() + " — " + session.Profile.Serial });
            devices.SelectedIndex = 0; devices.IsEnabled = false;
        }
        device.Children.Add(summary);
        device.Children.Add(new TextBlock { Text = "A device can be owned by one USB controller at a time. Close its vendor driver or Alpaca connection before connecting here.", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 20, 0, 8) });
        device.Children.Add(new TextBlock { Text = "Settings: " + session.ProfilePath, TextWrapping = TextWrapping.Wrap, FontSize = 12 });
        var motion = Page(tabs, "Motion");
        motion.Children.Add(Label(session.Kind == "efw" ? "Filter slot (1-based display)" : "Absolute position (steps)"));
        motion.Children.Add(target);
        moveButton = Button("Move", () => { session.Move(Parse(target) - (session.Kind == "efw" ? 1 : 0)); RefreshStatus(); });
        moveButton.IsEnabled = false; motion.Children.Add(moveButton);
        motion.Children.Add(motionStatus);
        if (session.Kind != "efw") {
            motion.Children.Add(Button("Halt", session.Halt));
            motion.Children.Add(new TextBlock { Text = "Check mechanical clearance before moving. The driver travel limit is enforced before a move. Focus step size in microns depends on the attached focuser.", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 20, 0, 0) });
            var settings = Page(tabs, "Settings"); if (session.Kind == "eaf") settings.Children.Add(beep); settings.Children.Add(reverse);
            settings.Children.Add(Label(session.Kind == "fc3" ? "Hardware backlash (0–1000 steps)" : "Hardware backlash (0–255 steps)")); settings.Children.Add(backlash);
            if (session.Kind == "fc3") { settings.Children.Add(Label("Motor speed (even values, 2–400)")); settings.Children.Add(speed); settings.Children.Add(Label("Driver travel range: 0–1,000,000 steps")); }
            else { settings.Children.Add(Label("Maximum travel (steps)")); settings.Children.Add(limit); }
            settings.Children.Add(Button("Apply settings", () => { if (session.Kind == "fc3") session.Request(new { command = "settings", speed = Parse(speed), backlash = Parse(backlash), reverse = reverse.IsChecked == true }); else session.Request(new { command = "settings", beep = beep.IsChecked == true, reverse = reverse.IsChecked == true, backlash = Parse(backlash), max_step = Parse(limit) }); initialized = false; RefreshStatus(); }));
            settings.Children.Add(new TextBlock { Text = "Set hardware backlash to zero if NINA handles backlash compensation. Temperature compensation is controlled by the imaging application.", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 20, 0, 0) });
        } else {
            motion.Children.Add(Label("Wheel calibration"));
            motion.Children.Add(new TextBlock { Text = "Rotate the wheel to detect all filter slots. Takes about 50 seconds and finishes at slot 1. Wait for completion before disconnecting; the wheel has no halt command.", TextWrapping = TextWrapping.Wrap });
            calibrateButton = Button("Calibrate wheel", () => { session.Calibrate(); RefreshStatus(); });
            calibrateButton.IsEnabled = false; motion.Children.Add(calibrateButton);
            var settings = Page(tabs, "Filters"); settings.Children.Add(filters);
            var direction = new CheckBox { Content = "Move in one direction", IsChecked = session.Profile.Unidirectional, Margin = new Thickness(0, 12, 0, 8) };
            settings.Children.Add(direction);
            settings.Children.Add(Button("Save filter settings", () => {
                if (session.Status().Moving) throw new InvalidOperationException("Wait for the wheel to stop");
                var names = filterRows.Select(r => r.Name.Text.Trim()).ToArray();
                var offsets = filterRows.Select(r => Parse(r.Offset)).ToArray();
                if (names.Any(string.IsNullOrEmpty) || !offsets.Contains(0)) throw new ArgumentException("Name each filter and keep at least one focus offset at zero");
                session.Profile.Names = names; session.Profile.FocusOffsets = offsets;
                session.Profile.Unidirectional = direction.IsChecked == true; session.Save();
            }));
        }
        Loaded += (_, _) => { if (session.Connected) Run(RefreshStatus); else Run(Discover); timer.Start(); };
        timer.Tick += (_, _) => { if (!busy && session.Connected) Run(RefreshStatus, quiet: true); else if (!session.Connected) { moveButton.IsEnabled = false; if (calibrateButton is not null) calibrateButton.IsEnabled = false; motionStatus.Text = "Disconnected"; } };
        Closed += (_, _) => { timer.Stop(); if (!externallyOwned && hasOtherClients?.Invoke() != true) session.Disconnect(); };
    }
    private void Discover() { devices.ItemsSource = session.Discover(); devices.SelectedItem = devices.Items.Cast<CaaChoice>().FirstOrDefault(v => v.Serial == session.Profile.Serial) ?? devices.Items.Cast<CaaChoice>().FirstOrDefault(); }
    private void RefreshStatus()
    {
        var s = session.Status();
        summary.Text = session.Kind == "efw" ? $"{s.Slots} slots  •  {(s.Moving ? "Moving" : "Slot " + (s.Position + 1))}" : $"Position {s.Position:N0} / {s.MaxStep:N0} steps  •  {(s.Moving ? "Moving" : "Idle")}\nTemperature: {(s.Temperature.HasValue ? s.Temperature.Value.ToString("F1") + " °C" : "Unavailable")}";
        if (s.Calibrating) summary.Text = $"Calibrating… {s.DetectedSlots ?? 0} slots detected. Waiting for the wheel to finish.";
        motionStatus.Text = summary.Text;
        moveButton.IsEnabled = !s.Moving;
        if (calibrateButton is not null) calibrateButton.IsEnabled = !s.Moving;
        if (!initialized) {
            target.Text = (s.Position + (session.Kind == "efw" ? 1 : 0)).ToString(CultureInfo.InvariantCulture);
            speed.Text = s.Speed.ToString(); beep.IsChecked = s.Beep; reverse.IsChecked = s.Reverse; backlash.Text = s.Backlash.ToString(); limit.Text = s.MaxStep.ToString();
            filters.Children.Clear(); filterRows.Clear();
            if (s.Slots > 0) filters.Children.Add(Row(new TextBlock { Width = 65 }, new TextBlock { Text = "Filter name", Width = 268, Margin = new Thickness(0, 0, 0, 8) }, new TextBlock { Text = "Focus offset", Width = 110 }));
            for (int i = 0; i < s.Slots; i++) {
                var name = new TextBox { Text = session.Profile.Names[i], Width = 260 };
                var offset = new TextBox { Text = session.Profile.FocusOffsets[i].ToString(), Width = 100 };
                filterRows.Add((name, offset)); filters.Children.Add(Row(new TextBlock { Text = "Slot " + (i + 1), Width = 65, VerticalAlignment = VerticalAlignment.Center }, name, offset));
            }
            initialized = true;
        }
    }
    private void Run(Action action, bool quiet = false)
    {
        if (busy) return;
        busy = true;
        try { action(); if (!quiet) message.Text = "Command accepted."; }
        catch (Exception error) { message.Text = error.Message; moveButton.IsEnabled = false; if (calibrateButton is not null) calibrateButton.IsEnabled = false; }
        finally { busy = false; }
    }
    private Button Button(string text, Action action) { var b = new Button { Content = text, Padding = new Thickness(14, 8, 14, 8), Margin = new Thickness(0, 8, 8, 4), HorizontalAlignment = HorizontalAlignment.Left }; b.Click += (_, _) => Run(action); return b; }
    private static int Parse(TextBox b) => int.Parse(b.Text, NumberStyles.Integer, CultureInfo.InvariantCulture);
    private static TextBlock Label(string text) => new() { Text = text, Margin = new Thickness(0, 12, 0, 6) };
    private static StackPanel Row(params UIElement[] controls) { var p = new StackPanel { Orientation = Orientation.Horizontal }; foreach (var c in controls) p.Children.Add(c); return p; }
    private static StackPanel Page(TabControl tabs, string title) { var p = new StackPanel { Margin = new Thickness(16) }; tabs.Items.Add(new TabItem { Header = title, Content = new ScrollViewer { Content = p, VerticalScrollBarVisibility = ScrollBarVisibility.Auto } }); return p; }
}
