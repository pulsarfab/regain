using System.Globalization;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Threading;

namespace Regain.Rotator;

public sealed class Ofp2SetupWindow : Window
{
    private readonly AccessorySession session;
    private readonly ComboBox devices = new() { MinWidth = 350 };
    private readonly TextBlock status = new() { TextWrapping = TextWrapping.Wrap };
    private readonly TextBlock coverStatus = new() { TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 12, 0, 0) };
    private readonly TextBlock message = new() { TextWrapping = TextWrapping.Wrap };
    private readonly TextBox brightness = new() { Text = "128", Width = 160, HorizontalAlignment = HorizontalAlignment.Left };
    private readonly DispatcherTimer timer = new() { Interval = TimeSpan.FromSeconds(1) };
    private bool busy;
    public static void Show(AccessorySession session, Func<bool> hasClients) => new Ofp2SetupWindow(session, hasClients).ShowDialog();
    public Ofp2SetupWindow(AccessorySession session, Func<bool> hasClients)
    {
        this.session = session;
        Title = "PulsarFab regain OFP2 Setup";
        Width = 730; Height = 610; MinWidth = 620; MinHeight = 540;
        WindowStartupLocation = WindowStartupLocation.CenterScreen;
        SetupTheme.Apply(this);
        var root = new DockPanel { Margin = new Thickness(24) }; Content = root;
        var heading = new StackPanel(); DockPanel.SetDock(heading, Dock.Top); root.Children.Add(heading);
        heading.Children.Add(new TextBlock { Text = "Deep Sky Dad OFP2", FontSize = 25 });
        heading.Children.Add(new TextBlock { Text = "PulsarFab regain  •  Native USB serial" + (RegainPaths.EnvironmentVariable("REGAIN_ACCESSORY_SIMULATE") == "1" ? "  •  Simulation" : ""), Margin = new Thickness(0, 8, 0, 20) });
        var footer = new StackPanel(); DockPanel.SetDock(footer, Dock.Bottom); root.Children.Add(footer);
        footer.Children.Add(message); footer.Children.Add(Button("Close", Close));
        var tabs = new TabControl(); root.Children.Add(tabs);
        var device = Page(tabs, "Device");
        device.Children.Add(Label("Choose a panel")); device.Children.Add(devices);
        device.Children.Add(Row(Button("Refresh", Discover), Button("Connect", () => {
            if (!session.Connected && devices.SelectedItem is CaaChoice choice) session.Select(choice.Serial);
            session.Connect(); RefreshStatus();
        }), Button("Disconnect", () => {
            if (hasClients()) throw new InvalidOperationException("Disconnect through the connected ASCOM applications first");
            session.Disconnect(); status.Text = "Disconnected";
        })));
        device.Children.Add(status);
        device.Children.Add(Label("ASCOM applications share one serial connection. Disconnect the vendor driver and Alpaca before using this driver."));
        var cover = Page(tabs, "Cover & light");
        cover.Children.Add(Label("Cover"));
        cover.Children.Add(Row(Button("Open cover", () => Command("open")), Button("Close cover", () => Command("close")), Button("Halt", () => Command("halt"))));
        cover.Children.Add(Label("Brightness (0–4096)")); cover.Children.Add(brightness);
        cover.Children.Add(Row(Button("Light on", () => {
            if (!int.TryParse(brightness.Text, NumberStyles.Integer, CultureInfo.InvariantCulture, out int value) || value < 0 || value > 4096) throw new ArgumentException("Brightness must be 0–4096");
            session.Request(new { command = "on", brightness = value }); RefreshStatus();
        }), Button("Light off", () => Command("off"))));
        cover.Children.Add(coverStatus);
        if (session.Connected) {
            devices.Items.Add(new CaaChoice { Serial = session.Profile.Serial, Label = "OFP2 — " + session.Profile.Serial }); devices.SelectedIndex = 0;
        }
        Loaded += (_, _) => Run(() => { if (session.Connected) RefreshStatus(); else Discover(); });
        timer.Tick += (_, _) => { if (session.Connected) Run(RefreshStatus); }; timer.Start();
        // The shared server owns connection lifetime, including clients that
        // connect while this modal dialog is open.
        Closed += (_, _) => timer.Stop();
    }
    private void Discover()
    {
        var choices = session.Discover(); devices.ItemsSource = null; devices.Items.Clear(); devices.ItemsSource = choices;
        devices.SelectedItem = choices.FirstOrDefault(c => c.Serial == session.Profile.Serial) ?? choices.FirstOrDefault();
        status.Text = choices.Count == 0 ? "No available OFP2 found. Check USB, power, and port ownership." : "Select a panel, then connect.";
    }
    private void RefreshStatus()
    {
        var s = Ofp2Status.Read(session);
        status.Text = $"Cover: {s.Cover}  •  Light: {(s.CalibratorOn ? "On" : "Off")}  •  Brightness: {(s.CalibratorOn ? s.Brightness : 0)} / {s.MaxBrightness}";
        coverStatus.Text = status.Text;
    }
    private void Command(string command) { session.Request(new { command }); RefreshStatus(); }
    private void Run(Action action) { if (busy) return; busy = true; try { action(); message.Text = ""; } catch (Exception e) { message.Text = e.Message; } finally { busy = false; } }
    private Button Button(string text, Action action) { var b = new Button { Content = text, Padding = new Thickness(14, 8, 14, 8), Margin = new Thickness(0, 8, 8, 4), HorizontalAlignment = HorizontalAlignment.Left }; b.Click += (_, _) => Run(action); return b; }
    private static TextBlock Label(string text) => new() { Text = text, TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 12, 0, 6) };
    private static StackPanel Row(params UIElement[] controls) { var row = new StackPanel { Orientation = Orientation.Horizontal }; foreach (var c in controls) row.Children.Add(c); return row; }
    private static StackPanel Page(TabControl tabs, string title) { var p = new StackPanel { Margin = new Thickness(16) }; tabs.Items.Add(new TabItem { Header = title, Content = new ScrollViewer { Content = p, VerticalScrollBarVisibility = ScrollBarVisibility.Auto } }); return p; }
}
