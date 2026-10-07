using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Threading;
using Regain.Hub;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    [Theory]
    [InlineData("rotator")]
    [InlineData("filterwheel")]
    [InlineData("covercalibrator")]
    public async Task NativeWindowCreatesAndReloadsSimulatedTypedOutputFromSharedControls(string deviceType)
    {
        await Wpf(async () => {
            await using var host = await Host.Open();
            var window = new HubConfigurationWindow(host.Executable, host.ConfigPath, host.Selection(0, "switch").InstanceId);
            try {
                window.Show();
                var review = Controls<Button>(window).Single(b => (string)b.Content == "Review changes");
                var apply = Controls<Button>(window).Single(b => (string)b.Content == "Apply reviewed configuration");
                await UiUntil(() => review.IsEnabled);
                Expander Group(string name) => Controls<Expander>(window).Single(e => (string)e.Header == name);
                void Click(Button button) => button.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                void Choose(string path, string choice) {
                    var box = Controls<ComboBox>(window).Single(c => (string?)c.Tag == path);
                    box.SelectedItem = box.Items.OfType<ComboBoxItem>().Single(c => (string?)c.Tag == choice);
                }
                void Text(string path, string value) => Controls<TextBox>(window).Single(c => (string?)c.Tag == path).Text = value;
                Group("Sources").IsExpanded = true;
                Click(Controls<Button>(Group("Sources")).Last(b => (string)b.Content == "Add item"));
                var source = Controls<Expander>(Group("Sources")).Last(); source.IsExpanded = true;
                Controls<Expander>(source).Single(e => (string)e.Header == "Backend").IsExpanded = true;
                Choose("/sources/3/backend", "simulated");
                Choose("/sources/3/backend/deviceType", deviceType);
                Text("/sources/3/label", "Explicit simulation " + deviceType + " setup");
                Group("Sources").IsExpanded = false;
                Group("Outputs").IsExpanded = true;
                Click(Controls<Button>(Group("Outputs")).Last(b => (string)b.Content == "Add item"));
                var output = Controls<Expander>(Group("Outputs")).Last(); output.IsExpanded = true;
                Controls<Expander>(output).Single(e => (string)e.Header == "Device").IsExpanded = true;
                Choose("/outputs/3/device", "proxy");
                Choose("/outputs/3/device/deviceType", deviceType);
                var references = Controls<ComboBox>(window).Single(c => (string?)c.Tag == "/outputs/3/device/source");
                references.SelectedItem = references.Items.OfType<ComboBoxItem>().Single(c => ((string)c.Content).StartsWith("Explicit simulation " + deviceType + " setup (", StringComparison.Ordinal));
                Text("/outputs/3/number", "7"); Text("/outputs/3/label", "Shared " + deviceType + " [SIMULATION]");
                Click(review); await UiUntil(() => apply.IsEnabled);
                Click(apply); await UiUntil(() => review.IsEnabled && !apply.IsEnabled);
                var saved = await host.Command(new { op = "getConfig" });
                var savedSource = saved.GetProperty("sources")[3]; var savedOutput = saved.GetProperty("outputs")[3];
                Assert.Equal(deviceType, savedSource.GetProperty("backend").GetProperty("deviceType").GetString());
                Assert.Equal(deviceType, savedOutput.GetProperty("device").GetProperty("deviceType").GetString());
                Assert.Equal(savedSource.GetProperty("id").GetGuid(), savedOutput.GetProperty("device").GetProperty("source").GetGuid());
                Assert.Equal(7, savedOutput.GetProperty("number").GetInt32());
                var reload = Controls<Button>(window).Single(b => (string)b.Content == "Reload saved configuration");
                Click(reload); await UiUntil(() => review.IsEnabled);
                Assert.Equal(savedOutput.GetProperty("id").GetGuid(), (await host.Command(new { op = "getConfig" })).GetProperty("outputs")[3].GetProperty("id").GetGuid());
                foreach (var item in saved.GetProperty("sources").EnumerateArray())
                    Assert.Equal(0, (await host.Command(new { op = "sourceStatus", source = item.GetProperty("id").GetGuid() })).GetProperty("leaseCount").GetInt32());
                Controls<TabControl>(window).Single().SelectedIndex = 0;
                Group("Outputs").IsExpanded = true; Group("Shared " + deviceType + " [SIMULATION]").IsExpanded = true;
                Controls<Expander>(Group("Shared " + deviceType + " [SIMULATION]")).Single(e => (string)e.Header == "Device").IsExpanded = true;
                var savedReference = Controls<ComboBox>(window).Single(c => (string?)c.Tag == "/outputs/3/device/source");
                Assert.Equal(savedSource.GetProperty("id").GetString(), (string)((ComboBoxItem)savedReference.SelectedItem).Tag);
                await window.Dispatcher.InvokeAsync(() => { }, DispatcherPriority.ContextIdle);
                window.UpdateLayout();
                Assert.True(savedReference.IsVisible); savedReference.BringIntoView();
                await Capture(window, "hub-native-" + deviceType + "-setup-simulation.png");
            } finally { window.Close(); }
        });
    }
    [Fact]
    public async Task NativeEditorWindowEditsReviewsAppliesAndShowsSavedHealth()
    {
        await Wpf(async () => {
            await using var host = await Host.Open();
            var window = new HubConfigurationWindow(host.Executable, host.ConfigPath, host.Selection(0, "switch").InstanceId);
            try {
                window.Show();
                var review = Controls<Button>(window).Single(b => (string)b.Content == "Review changes");
                var apply = Controls<Button>(window).Single(b => (string)b.Content == "Apply reviewed configuration");
                await UiUntil(() => review.IsEnabled);
                var outputs = Controls<Expander>(window).Single(e => (string)e.Header == "Outputs"); outputs.IsExpanded = true;
                Controls<Expander>(outputs).Single(e => (string)e.Header == "Simulation controls").IsExpanded = true;
                var label = Controls<TextBox>(outputs).Single(c => (string?)c.Tag == "/outputs/0/label");
                label.Text = "Native setup simulation";
                Assert.False(apply.IsEnabled);
                await Capture(window, "hub-native-editor-simulation.png");
                review.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(() => apply.IsEnabled);
                Assert.Contains("Native setup simulation", Controls<TextBox>(window).Single(t => t.IsReadOnly && t.FontFamily.Source == "Consolas").Text);
                await Capture(window, "hub-native-review-simulation.png");
                apply.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(() => review.IsEnabled && !apply.IsEnabled);
                var saved = await host.Command(new { op = "getConfig" });
                Assert.Equal("Native setup simulation", saved.GetProperty("outputs")[0].GetProperty("label").GetString());
                var tabs = Controls<TabControl>(window).Single(); tabs.SelectedIndex = 2;
                var status = Controls<Button>(window).Single(b => (string)b.Content == "Saved host status");
                status.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                Assert.Contains(saved.GetProperty("revision").GetString()!, Controls<TextBox>(window).Single(t => (string?)t.Tag == "source-result").Text);
                await Capture(window, "hub-native-health-simulation.png");
                var read = Controls<Button>(window).Single(b => (string)b.Content == "Read cached source health");
                read.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(() => read.IsEnabled);
                using var health = JsonDocument.Parse(Controls<TextBox>(window).Single(t => (string?)t.Tag == "source-result").Text);
                Assert.Equal(0, health.RootElement.GetProperty("leaseCount").GetInt32());
                window.Close();
                Assert.Equal(saved.GetProperty("revision").GetGuid(), (await host.Command(new { op = "getConfig" })).GetProperty("revision").GetGuid());
                for (int i = 0; i < 3; i++) Assert.Equal(0, (await host.Status(i)).GetProperty("leaseCount").GetInt32());
            } finally { window.Close(); }
        });
    }
    private static async Task UiUntil(Func<bool> predicate)
    {
        var deadline = DateTime.UtcNow.AddSeconds(10);
        while (!predicate()) {
            if (DateTime.UtcNow > deadline) throw new TimeoutException("Native setup did not reach its expected UI state");
            await Task.Delay(20);
        }
    }
    private static IEnumerable<T> Controls<T>(DependencyObject root) where T : DependencyObject
    {
        if (root is T match) yield return match;
        foreach (var child in LogicalTreeHelper.GetChildren(root).OfType<DependencyObject>())
            foreach (var item in Controls<T>(child)) yield return item;
    }
    private static async Task Capture(Window window, string name)
    {
        var directory = Environment.GetEnvironmentVariable("REGAIN_HUB_SCREENSHOT_DIR");
        if (string.IsNullOrEmpty(directory)) return;
        await window.Dispatcher.InvokeAsync(() => { }, DispatcherPriority.ContextIdle);
        window.UpdateLayout();
        var bitmap = new RenderTargetBitmap((int)window.ActualWidth, (int)window.ActualHeight, 96, 96, PixelFormats.Pbgra32);
        // A Window owns an HWND, so RenderTargetBitmap cannot capture its native
        // wrapper. Render the real laid-out WPF content and background instead.
        var content = (FrameworkElement)window.Content;
        var drawing = new DrawingVisual();
        using (var context = drawing.RenderOpen()) {
            context.DrawRectangle(window.Background ?? Brushes.White, null, new Rect(0, 0, bitmap.PixelWidth, bitmap.PixelHeight));
            context.DrawRectangle(new VisualBrush(content), null, new Rect(16, 16, content.ActualWidth, content.ActualHeight));
        }
        bitmap.Render(drawing);
        var pixels = new byte[bitmap.PixelWidth * bitmap.PixelHeight * 4];
        bitmap.CopyPixels(pixels, bitmap.PixelWidth * 4, 0);
        var encoder = new PngBitmapEncoder(); encoder.Frames.Add(BitmapFrame.Create(bitmap));
        Directory.CreateDirectory(directory);
        using var stream = File.Create(Path.Combine(directory, name)); encoder.Save(stream);
        Assert.True(pixels.Distinct().Count() > 16, "The native setup render was empty: " + string.Join(",", pixels.Distinct()) + "; content " + content.ActualWidth + "x" + content.ActualHeight + "; drawing " + drawing.Drawing.Bounds);
    }
    private static Task Wpf(Func<Task> action)
    {
        var completed = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        var thread = new Thread(() => {
            var dispatcher = Dispatcher.CurrentDispatcher;
            SynchronizationContext.SetSynchronizationContext(new DispatcherSynchronizationContext(dispatcher));
            dispatcher.BeginInvoke(new Action(async () => {
                try { await action(); completed.TrySetResult(true); }
                catch (Exception error) { completed.TrySetException(error); }
                finally { dispatcher.BeginInvokeShutdown(DispatcherPriority.Background); }
            }));
            Dispatcher.Run();
        });
        thread.SetApartmentState(ApartmentState.STA); thread.IsBackground = true; thread.Start(); return completed.Task;
    }
}
