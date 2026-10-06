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
                Assert.Contains(saved.GetProperty("revision").GetString()!, Controls<TextBox>(window).Single(t => t.IsReadOnly && t.FontFamily.Source != "Consolas").Text);
                await Capture(window, "hub-native-health-simulation.png");
                var read = Controls<Button>(window).Single(b => (string)b.Content == "Read cached source health");
                read.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(() => read.IsEnabled);
                using var health = JsonDocument.Parse(Controls<TextBox>(window).Single(t => t.IsReadOnly && t.FontFamily.Source != "Consolas").Text);
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
