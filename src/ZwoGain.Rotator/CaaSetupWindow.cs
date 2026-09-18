using System.Globalization;
using System.Text.Json;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Threading;
using System.Windows.Media;

namespace ZwoGain.Rotator;

public static class CaaSetupWindow
{
    public static void Show(CaaSession session, bool ownedByApplication)
    {
        var dispatcher = Application.Current?.Dispatcher;
        if (dispatcher is not null && !dispatcher.CheckAccess()) {
            dispatcher.Invoke(() => Show(session, ownedByApplication)); return;
        }
        bool busy = false, polling = false, closed = false, moving = false;
        var connectedControls = new List<Control>();
        var idleControls = new List<Control>();
        var offlineControls = new List<Control>();
        var root = new DockPanel { Margin = new Thickness(16) };
        var heading = new TextBlock { Text = "CAA rotator", FontSize = 18, FontWeight = FontWeights.SemiBold, Margin = new Thickness(0, 0, 0, 12) };
        DockPanel.SetDock(heading, Dock.Top); root.Children.Add(heading);
        var footer = new StackPanel { Margin = new Thickness(0, 12, 0, 0) };
        DockPanel.SetDock(footer, Dock.Bottom); root.Children.Add(footer);
        var live = new TextBlock { Text = "Disconnected", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 0, 0, 8) };
        var feedback = new TextBlock { TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 0, 0, 8) };
        feedback.SetResourceReference(TextBlock.ForegroundProperty, "PrimaryBrush");
        footer.Children.Add(live); footer.Children.Add(feedback);
        var bottom = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right };
        footer.Children.Add(bottom);
        var tabs = new TabControl(); root.Children.Add(tabs);
        var window = new Window {
            Title = "ZWOgain CAA Setup", Width = 720, MinWidth = 580,
            Height = Math.Min(540, SystemParameters.WorkArea.Height * .9),
            MaxHeight = SystemParameters.WorkArea.Height * .9,
            Content = root, Owner = Application.Current?.MainWindow,
            WindowStartupLocation = WindowStartupLocation.CenterOwner
        };
        // Use the host's theme in NINA; standalone ASCOM clients get a light theme.
        if (Application.Current?.TryFindResource("BackgroundBrush") is null) {
            window.Resources["BackgroundBrush"] = new SolidColorBrush(Color.FromRgb(245, 247, 248));
            window.Resources["PrimaryBrush"] = new SolidColorBrush(Color.FromRgb(32, 47, 53));
            window.FontFamily = new FontFamily("Segoe UI"); window.FontSize = 14;
            var buttons = new Style(typeof(Button));
            buttons.Setters.Add(new Setter(Control.BackgroundProperty, new SolidColorBrush(Color.FromRgb(229, 237, 239))));
            buttons.Setters.Add(new Setter(Control.BorderThicknessProperty, new Thickness(0)));
            var border = new FrameworkElementFactory(typeof(Border));
            border.SetBinding(Border.BackgroundProperty, new System.Windows.Data.Binding("Background") { RelativeSource = System.Windows.Data.RelativeSource.TemplatedParent });
            border.SetBinding(Border.PaddingProperty, new System.Windows.Data.Binding("Padding") { RelativeSource = System.Windows.Data.RelativeSource.TemplatedParent });
            border.SetValue(Border.CornerRadiusProperty, new CornerRadius(3));
            var content = new FrameworkElementFactory(typeof(ContentPresenter));
            content.SetValue(FrameworkElement.HorizontalAlignmentProperty, HorizontalAlignment.Center);
            content.SetValue(FrameworkElement.VerticalAlignmentProperty, VerticalAlignment.Center);
            border.AppendChild(content);
            buttons.Setters.Add(new Setter(Control.TemplateProperty, new ControlTemplate(typeof(Button)) { VisualTree = border }));
            var disabled = new Trigger { Property = UIElement.IsEnabledProperty, Value = false };
            disabled.Setters.Add(new Setter(UIElement.OpacityProperty, .45)); buttons.Triggers.Add(disabled);
            var hover = new Trigger { Property = UIElement.IsMouseOverProperty, Value = true };
            hover.Setters.Add(new Setter(Control.BackgroundProperty, new SolidColorBrush(Color.FromRgb(204, 224, 226)))); buttons.Triggers.Add(hover);
            window.Resources[typeof(Button)] = buttons;
            var tabStyle = new Style(typeof(TabItem)); tabStyle.Setters.Add(new Setter(Control.PaddingProperty, new Thickness(10, 6, 10, 6)));
            window.Resources[typeof(TabItem)] = tabStyle;
        }
        window.SetResourceReference(Window.BackgroundProperty, "BackgroundBrush");
        window.SetResourceReference(Window.ForegroundProperty, "PrimaryBrush");
        StackPanel Tab(string title, string description) {
            var body = new StackPanel { Margin = new Thickness(12) };
            body.Children.Add(new TextBlock { Text = description, TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 0, 0, 18) });
            tabs.Items.Add(new TabItem { Header = title, Content = new ScrollViewer { Content = body,
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled } });
            return body;
        }
        void UpdateControls() {
            bool connected = session.Connected;
            foreach (var control in connectedControls) control.IsEnabled = connected && !busy;
            foreach (var control in idleControls) control.IsEnabled = connected && !busy && !moving;
            foreach (var control in offlineControls) control.IsEnabled = !connected && !busy;
        }
        async Task Run(Func<Task> action) {
            if (busy) return;
            busy = true; feedback.Text = ""; UpdateControls();
            try { await action(); }
            catch (Exception error) { if (!closed) feedback.Text = error.GetBaseException().Message; }
            finally { busy = false; if (!closed) UpdateControls(); }
        }
        Button Button(Panel parent, string title, Func<Task> action, List<Control>? group = null) {
            var button = new Button { Content = title, HorizontalAlignment = HorizontalAlignment.Left,
                Padding = new Thickness(12, 6, 12, 6), MinWidth = 90, Margin = new Thickness(0, 0, 8, 10) };
            parent.Children.Add(button); group?.Add(button);
            button.Click += async (_, _) => await Run(action);
            return button;
        }
        TextBox Field(Panel parent, string label, string value, string? hint = null) {
            var row = new Grid { Margin = new Thickness(0, 0, 0, 12) };
            row.ColumnDefinitions.Add(new ColumnDefinition()); row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(130) });
            row.Children.Add(new TextBlock { Text = label, TextWrapping = TextWrapping.Wrap,
                VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(0, 0, 16, 0) });
            var input = new TextBox { Text = value, MinHeight = 28, VerticalContentAlignment = VerticalAlignment.Center, ToolTip = hint };
            AutomationProperties.SetName(input, label); Grid.SetColumn(input, 1); row.Children.Add(input); parent.Children.Add(row);
            idleControls.Add(input); return input;
        }
        static double Value(TextBox box, double min, double max, bool integer = false) {
            if (!double.TryParse(box.Text, NumberStyles.Float, CultureInfo.InvariantCulture, out double value) ||
                (double.IsNaN(value) || double.IsInfinity(value)) || value < min || value > max || (integer && value != Math.Truncate(value)))
                throw new ArgumentException($"{AutomationProperties.GetName(box)} must be {(integer ? "a whole number " : "")}between {min} and {max}.");
            return value;
        }
        Task Command(string name, double value) => Task.Run(() => session.Command(name, value));
        Task Action(string name, object args) => Task.Run(() => session.Action("ZwoGain.CAA." + name, JsonSerializer.Serialize(args)));

        var device = Tab("Device", "Choose a rotator. Your selection is saved automatically.");
        var picker = new ComboBox { DisplayMemberPath = nameof(CaaChoice.Label), MinHeight = 30, Margin = new Thickness(0, 0, 0, 12) };
        AutomationProperties.SetName(picker, "CAA rotator"); device.Children.Add(picker); offlineControls.Add(picker);
        if (!string.IsNullOrEmpty(session.Profile.Serial)) {
            picker.Items.Add(new CaaChoice { Serial = session.Profile.Serial, Label = "CAA — " + session.Profile.Serial + " (saved)" }); picker.SelectedIndex = 0;
        }
        var deviceActions = new WrapPanel(); device.Children.Add(deviceActions);
        async Task Discover() {
            var choices = await Task.Run(session.Discover);
            if (closed) return;
            var saved = picker.SelectedItem as CaaChoice;
            if (saved is not null && choices.All(c => c.Serial != saved.Serial)) choices.Insert(0, saved);
            picker.Items.Clear(); foreach (var choice in choices) picker.Items.Add(choice);
            picker.SelectedItem = choices.FirstOrDefault(c => c.Serial == saved?.Serial) ?? (choices.Count == 1 ? choices[0] : null);
            if (picker.SelectedItem is CaaChoice) SaveSelection();
            if (choices.Count == 0) feedback.Text = "No CAA found. Check USB and close other rotator controllers.";
        }
        void SaveSelection() => session.Select((picker.SelectedItem as CaaChoice)?.Serial ?? "");
        Button(deviceActions, "Refresh", Discover, offlineControls);
        picker.SelectionChanged += (_, _) => {
            if (busy || session.Connected || picker.SelectedItem is not CaaChoice) return;
            try { SaveSelection(); feedback.Text = ""; } catch (Exception error) { feedback.Text = error.Message; }
        };
        var identity = new TextBlock { TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 12, 0, 18) }; device.Children.Add(identity);

        var motion = Tab("Motion", "Move within the configured travel limit.");
        var absolute = Field(motion, "Mechanical angle (°)", "0");
        Button(motion, "Move mechanical", () => Command("move-mechanical", Value(absolute, 0, 361)), idleControls);
        var relative = Field(motion, "Relative sky angle (°)", "1");
        Button(motion, "Move relative", () => Command("move-relative", Value(relative, -360, 360)), idleControls);
        var sky = Field(motion, "Sync sky angle (°)", "0", "Changes sky coordinates without moving or changing mechanical zero.");
        Button(motion, "Sync sky angle", () => { double value = Value(sky, 0, 359.99); return Task.Run(() => session.Sync(value)); }, idleControls);

        var settings = Tab("Settings", "Read from the rotator when connected.");
        CheckBox Toggle(string label) {
            settings.Children.Add(new TextBlock { Text = label });
            var toggle = new CheckBox { HorizontalAlignment = HorizontalAlignment.Left, Margin = new Thickness(0, 4, 0, 16) };
            AutomationProperties.SetName(toggle, label); settings.Children.Add(toggle); idleControls.Add(toggle); return toggle;
        }
        var beep = Toggle("Beep"); var reverse = Toggle("Reverse sky direction");
        var alias = Field(settings, "Device alias", "", "Up to eight printable ASCII characters."); alias.MaxLength = 8;
        Button(settings, "Apply settings", async () => {
            bool b = beep.IsChecked == true, r = reverse.IsChecked == true; string name = alias.Text;
            if (name.Any(c => c < 32 || c > 126)) throw new ArgumentException("Use printable ASCII characters for the device alias.");
            await Task.Run(() => { session.Request(new { command = "beep", enabled = b }); session.Request(new { command = "reverse", enabled = r });
                session.RememberCoordinates(); session.Request(new { command = "alias", text = name }); });
            feedback.Text = "Settings applied.";
        }, idleControls);

        var reference = Tab("Reference", "Relabel the current position without moving. This changes where the travel limit begins.");
        Button(reference, "Set current position to mechanical 0°", () => Task.Run(() => session.Action("ZwoGain.CAA.ResetOrigin", "")), idleControls);
        var origin = Field(reference, "Mechanical reference (°)", "0");
        Button(reference, "Set reference", () => Action("SetReference", new { degrees = Value(origin, 0, 360) }), idleControls);
        var limit = Field(reference, "Travel limit (°)", "360", "360° is standard; 361° is experimental.");
        Button(reference, "Set travel limit", () => Action("SetLimit", new { degrees = (int)Value(limit, 1, 361, true) }), idleControls);
        reference.Children.Add(new TextBlock { Text = "360° is standard. 361° is experimental.", TextWrapping = TextWrapping.Wrap });

        var multi = Tab("Multi-turn", "Travel in segments of up to 90°, resetting the mechanical reference between segments. Positive and negative values select physical direction.");
        multi.Children.Add(new TextBlock { Text = "This bypasses cable-wrap protection. Allow clearance and cable slack for the whole move.", TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 0, 0, 16) });
        var travel = Field(multi, "Physical travel (°)", "90");
        multi.Children.Add(new TextBlock { Text = "Clearance and cable slack checked" });
        var clearance = new CheckBox { HorizontalAlignment = HorizontalAlignment.Left, Margin = new Thickness(0, 4, 0, 16) };
        AutomationProperties.SetName(clearance, "Clearance and cable slack checked"); multi.Children.Add(clearance); idleControls.Add(clearance);
        Button(multi, "Start travel", () => {
            double degrees = Value(travel, -450, 450);
            if (degrees == 0) throw new ArgumentException("Enter a nonzero travel angle.");
            if (clearance.IsChecked != true) throw new InvalidOperationException("Check clearance and cable slack first.");
            clearance.IsChecked = false; return Action("RotateUnwrapped", new { degrees });
        }, idleControls);
        var halt = new Button { Content = "Halt", MinWidth = 88, Padding = new Thickness(16, 6, 16, 6), Margin = new Thickness(0, 0, 8, 0) };
        bottom.Children.Add(halt);
        // Halt remains available while another operation awaits its USB response.
        halt.Click += async (_, _) => { try { await Task.Run(session.Halt); } catch (Exception error) { if (!closed) feedback.Text = error.GetBaseException().Message; } };
        var close = new Button { Content = "Close", MinWidth = 88, Padding = new Thickness(16, 6, 16, 6), IsCancel = true };
        close.Click += (_, _) => window.Close(); bottom.Children.Add(close);
        void Display(CaaStatus status) {
            moving = status.Moving;
            live.Text = $"Mechanical {status.Mechanical:F2}°    Sky {status.Logical:F2}°    {(moving ? "Moving" : "Idle")}";
            if (status.MotionError is not null || status.Error != 0) feedback.Text = status.MotionError ?? "CAA fault " + status.Error;
        }
        async Task ReadDetails() {
            var details = await Task.Run(() => (Identity: session.Request(new { command = "identity" }), Settings: session.Request(new { command = "settings" }), Status: session.Status()));
            if (closed) return;
            var id = details.Identity;
            identity.Text = $"{id.GetProperty("model").GetString()} · Firmware {string.Join(".", id.GetProperty("firmware").EnumerateArray().Select(v => v.GetInt32()))}\nSerial {id.GetProperty("serial").GetString()}";
            beep.IsChecked = details.Settings.GetProperty("beep").GetBoolean(); reverse.IsChecked = details.Settings.GetProperty("reverse").GetBoolean(); alias.Text = id.GetProperty("alias").GetString();
            absolute.Text = origin.Text = details.Status.Mechanical.ToString("F2", CultureInfo.InvariantCulture);
            sky.Text = details.Status.Logical.ToString("F2", CultureInfo.InvariantCulture); limit.Text = details.Status.Limit.ToString(CultureInfo.InvariantCulture); Display(details.Status);
        }
        if (ownedByApplication) device.Children.Add(new TextBlock { Text = "Connected through your application. Disconnect there when finished.", TextWrapping = TextWrapping.Wrap });
        else {
            Button(deviceActions, "Connect", async () => { SaveSelection(); await Task.Run(session.Connect); await ReadDetails(); }, offlineControls);
            Button(deviceActions, "Disconnect", async () => { await Task.Run(session.Disconnect); moving = false; live.Text = "Disconnected"; }, connectedControls);
        }
        Button(deviceActions, "Read device", ReadDetails, connectedControls);
        var timer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(750) };
        timer.Tick += async (_, _) => {
            if (closed || polling || busy) return;
            halt.IsEnabled = session.Connected;
            if (!session.Connected) return;
            polling = true;
            try { var state = await Task.Run(session.Status); if (!closed) Display(state); }
            catch (Exception error) { if (!closed) feedback.Text = error.GetBaseException().Message; }
            finally { polling = false; if (!closed) UpdateControls(); }
        };
        window.Closing += (_, e) => { if (busy) { e.Cancel = true; feedback.Text = "Waiting for the current command."; } };
        window.Closed += (_, _) => { closed = true; timer.Stop(); };
        window.Loaded += async (_, _) => { await Run(session.Connected ? ReadDetails : Discover); if (!closed) timer.Start(); };
        UpdateControls(); halt.IsEnabled = session.Connected;
        window.ShowDialog();
    }
}
