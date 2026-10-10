using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using Regain.Hub;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    [Fact]
    public async Task NativeLocalCatalogUsesProductionSimulationAndAddsOnlyADraftSource() {
        await Wpf(async () => {
            await using var host = await Host.Open(nativeSimulation: true);
            var window = new HubConfigurationWindow(host.Executable, host.ConfigPath, host.Selection(0, "switch").InstanceId);
            try {
                window.Height = 820; window.Show();
                await UiUntil(() => Controls<Button>(window).Single(b => (string)b.Content == "Review changes").IsEnabled);
                var tabs = Controls<TabControl>(window).Single(); tabs.SelectedItem = tabs.Items.Cast<TabItem>().Single(t => (string)t.Header == "Discover devices");
                var backend = Controls<ComboBox>(window).Single(b => (string?)b.Tag == "local-discovery-device"); backend.SelectedItem = "eaf";
                var query = Controls<Button>(window).Single(b => (string)b.Content == "Read local device catalog");
                query.BringIntoView(); query.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                var result = Controls<TextBox>(window).Single(box => (string?)box.Tag == "local-discovery-result");
                await UiUntil(() => query.IsEnabled && result.Text.Contains("0102030405060709"), () => WindowState(window, "local catalog") + " " + result.Text);
                Assert.Contains("SIMULATION", result.Text);
                var selection = Controls<ComboBox>(window).Single(b => (string?)b.Tag == "local-discovery-selection"); Assert.Single(selection.Items.Cast<object>());
                selection.SelectedIndex = 0;
                var add = Controls<Button>(window).Single(b => (string)b.Content == "Add local source to draft"); Assert.True(add.IsEnabled);
                add.BringIntoView(); await Capture(window, "hub-native-local-discovery-simulation.png");
                add.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(() => query.IsEnabled);
                var saved = await host.Command(new { op = "getConfig" }); Assert.Equal(3, saved.GetProperty("sources").GetArrayLength());
                for (int i = 0; i < 3; i++) Assert.Equal(0, (await host.Status(i)).GetProperty("leaseCount").GetInt32());
                tabs.SelectedIndex = 0;
                Controls<Expander>(window).Single(e => (string)e.Header == "Sources").IsExpanded = true;
                Assert.Single(Controls<Expander>(window), e => (string)e.Header == "EAFN");
            } finally { window.Close(); }
        });
    }
    [Fact]
    public async Task LocalIdentityProbeRejectsConnectedOutputsBeforeStartingWorkers() {
        await using var host = await Host.Open(nativeSimulation: true);
        var saved = await host.Command(new { op = "getConfig" });
        await host.Command(new { op = "connect", output = host.Selection(0, "switch").OutputId });
        var error = await Assert.ThrowsAsync<HubException>(() => host.Command(new { op = "discoverLocal", target = new { kind = "native", device = "eaf" }, expectedRevision = saved.GetProperty("revision").GetGuid() }));
        Assert.Equal("busy", error.Remote!.Code); Assert.Equal(1, (await host.Status(0)).GetProperty("leaseCount").GetInt32());
    }
}
