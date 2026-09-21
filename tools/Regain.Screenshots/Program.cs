// Render the production WPF controls offscreen. No desktop capture or mock UI.
using System.IO;
using System.Reflection;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Documents;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using Regain.Rotator;

internal static class Program
{
    [STAThread]
    static void Main(string[] args)
    {
        string root = Path.GetFullPath(args.Length > 0 ? args[0] : ".");
        string output = Path.Combine(root, "docs", "images"); Directory.CreateDirectory(output);
        string profiles = Path.Combine(root, "artifacts", "native-screenshots", Guid.NewGuid().ToString("N"));
        RenderOptions.ProcessRenderMode = System.Windows.Interop.RenderMode.SoftwareOnly;
        if (args.Contains("--ofp2")) {
            Environment.SetEnvironmentVariable("REGAIN_ACCESSORY_SIMULATE", "1");
            using var panel = new AccessorySession(Path.Combine(root, "target", "debug", "regain-ofp2.exe"), "ofp2", Path.Combine(profiles, "ofp2.json"));
            panel.Connect();
            var window = new Ofp2SetupWindow(panel, () => true);
            typeof(Ofp2SetupWindow).GetMethod("RefreshStatus", BindingFlags.Instance | BindingFlags.NonPublic)!.Invoke(window, null);
            var content = (DockPanel)window.Content; window.Content = null;
            var tabs = content.Children.OfType<TabControl>().Single();
            var devicePage = (StackPanel)((ScrollViewer)((TabItem)tabs.Items[0]).Content).Content;
            var disconnect = devicePage.Children.OfType<StackPanel>().SelectMany(p => p.Children.OfType<Button>()).Single(b => (string)b.Content == "Disconnect");
            disconnect.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            if (!panel.Connected) throw new IOException("Setup disconnected an ASCOM client's shared connection");
            tabs.SelectedIndex = 1;
            var controls = (StackPanel)((ScrollViewer)((TabItem)tabs.Items[1]).Content).Content;
            var buttons = controls.Children.OfType<StackPanel>().SelectMany(p => p.Children.OfType<Button>()).ToList();
            buttons.Single(b => (string)b.Content == "Light on").RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            if (!Ofp2Status.Read(panel).CalibratorOn || Ofp2Status.Read(panel).Brightness != 128) throw new IOException("Setup light-on command failed");
            buttons.Single(b => (string)b.Content == "Light off").RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            if (Ofp2Status.Read(panel).CalibratorOn) throw new IOException("Setup light-off command failed");
            var border = new Border { Background = window.Background, Child = content, Resources = window.Resources };
            border.SetValue(TextElement.FontFamilyProperty, window.FontFamily);
            border.SetValue(TextElement.FontSizeProperty, window.FontSize);
            border.SetValue(TextElement.ForegroundProperty, window.Foreground);
            Render(border, Path.Combine(output, "native-ofp2.png"));
            window.Close();
            if (!panel.Connected) throw new IOException("Closing setup released a connection owned by ASCOM");
            Console.WriteLine("OFP2 setup controls and shared-client protection passed (simulation).");
            return;
        }
        bool fc3=args.Contains("--fc3");
        Environment.SetEnvironmentVariable("REGAIN_ACCESSORY_SIMULATE", fc3 ? null : "1");
        foreach (string kind in (fc3 ? new[] {"fc3"} : new[] { "efw", "eaf" })) {
            using var session = new AccessorySession(Path.Combine(root, "target", "debug", (fc3 ? "regain-fc3.exe" : "regain-accessories.exe")), kind, Path.Combine(profiles, kind + ".json"));
            session.Connect();
            var window = new AccessorySetupWindow(session);
            // Run the same refresh used by the live dialog, without opening a desktop window.
            typeof(AccessorySetupWindow).GetMethod("RefreshStatus", BindingFlags.Instance | BindingFlags.NonPublic)!.Invoke(window, null);
            var content = (DockPanel)window.Content; window.Content = null;
            content.Children.OfType<TabControl>().Single().SelectedIndex = 2;
            var border = new Border { Background = window.Background, Child = content, Resources = window.Resources };
            border.SetValue(TextElement.FontFamilyProperty, window.FontFamily);
            border.SetValue(TextElement.FontSizeProperty, window.FontSize);
            border.SetValue(TextElement.ForegroundProperty, window.Foreground);
            Render(border, Path.Combine(output, "native-" + kind + ".png"));
            if (kind == "efw") {
                var tabs = content.Children.OfType<TabControl>().Single(); tabs.SelectedIndex = 1;
                Render(border, Path.Combine(output, "native-efw-calibration.png"));
                var panel = (StackPanel)((ScrollViewer)((TabItem)tabs.Items[1]).Content).Content;
                var calibrate = panel.Children.OfType<Button>().Single(b => (string)b.Content == "Calibrate wheel");
                if (!calibrate.IsEnabled) throw new IOException("Calibration button is disabled while idle");
                calibrate.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                if (!session.Status().Calibrating || calibrate.IsEnabled) throw new IOException("Calibration UI did not enter busy state");
                var deadline = DateTime.UtcNow.AddSeconds(10);
                while (session.Status().Calibrating) {
                    if (DateTime.UtcNow > deadline) throw new TimeoutException("UI calibration timed out");
                    Thread.Sleep(100);
                }
                typeof(AccessorySetupWindow).GetMethod("RefreshStatus", BindingFlags.Instance | BindingFlags.NonPublic)!.Invoke(window, null);
                if (!calibrate.IsEnabled || session.Status().Position != 0) throw new IOException("Calibration UI did not return to idle");
                Console.WriteLine("WPF calibration button and progress passed.");
            }
        }
    }
    private static void Render(Border border, string path)
    {
        border.Measure(new Size(730, 650)); border.Arrange(new Rect(0, 0, 730, 650)); border.UpdateLayout();
        var bitmap = new RenderTargetBitmap(730, 650, 96, 96, PixelFormats.Pbgra32); bitmap.Render(border);
        byte[] pixels = new byte[730 * 650 * 4]; bitmap.CopyPixels(pixels, 730 * 4, 0);
        if (pixels.All(b => b == 0)) throw new IOException("WPF produced an empty render");
        var encoder = new PngBitmapEncoder(); encoder.Frames.Add(BitmapFrame.Create(bitmap));
        using var file = File.Create(path); encoder.Save(file); Console.WriteLine(path);
    }
}
