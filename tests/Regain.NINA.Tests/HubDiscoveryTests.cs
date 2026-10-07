using System.Text.Json;
using System.Text.Json.Nodes;
using System.Windows;
using System.Windows.Controls;
using Regain.Hub;
using Regain.TestFixtures;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    [Fact]
    public async Task NativeDiscoveryUsesRealPrivateIpcWithoutOpeningSourcesOrChangingReview()
    {
        await using var host = await Host.Open(); using var editor = await Editor(host);
        await HubDiscoveryFixture.Run(editor);
    }
    [Theory]
    [InlineData("revision")]
    [InlineData("url")]
    [InlineData("extra")]
    [InlineData("missing")]
    [InlineData("number")]
    [InlineData("duplicate")]
    [InlineData("identity")]
    [InlineData("supported")]
    [InlineData("unsupported")]
    [InlineData("name")]
    [InlineData("lost")]
    public async Task NativeDiscoveryRejectsMalformedCatalogsAndRequiresReloadWithoutRetry(string fault)
    {
        var saved = HubDraftTests.Configuration(); var description = HubDraftTests.Description(); int requests = 0;
        using var editor = new HubEditorSession(saved.GetProperty("instanceId").GetGuid(), (command, _) => {
            switch (command.GetProperty("op").GetString()) {
                case "describeConfig": return Task.FromResult(description);
                case "getConfig": return Task.FromResult(saved);
                case "hostStatus": return Task.FromResult(JsonSerializer.SerializeToElement(new { phase = "ready", configurationRevision = saved.GetProperty("revision").GetGuid() }));
                case "discoverAlpaca":
                    requests++;
                    Assert.Equal(saved.GetProperty("revision").GetGuid(), command.GetProperty("expectedRevision").GetGuid());
                    Assert.Equal("protected-reference", command.GetProperty("credentialReference").GetString());
                    if (fault == "lost") throw new HubException(HubFailure.Disconnected);
                    var reply = JsonSerializer.SerializeToNode(new {
                        configurationRevision = saved.GetProperty("revision").GetGuid(), baseUrl = "http://localhost:11111/prefix",
                        devices = new[] { new { name = "[SIMULATION] camera", reportedDeviceType = "Camera", supportedDeviceType = "camera", number = uint.MaxValue, uniqueId = "camera unit 42" } }
                    })!.AsObject();
                    var device = reply["devices"]![0]!;
                    if (fault == "revision") reply["configurationRevision"] = Guid.NewGuid().ToString();
                    if (fault == "url") reply["baseUrl"] = "http://other.example:11111/prefix";
                    if (fault == "extra") reply["authorization"] = "must-not-escape";
                    if (fault == "missing") device.AsObject().Remove("uniqueId");
                    if (fault == "number") device["number"] = 4294967296uL;
                    if (fault == "duplicate") reply["devices"]!.AsArray().Add(device.DeepClone());
                    if (fault == "identity") device["uniqueId"] = "\ninvalid";
                    if (fault == "supported") device["supportedDeviceType"] = "focuser";
                    if (fault == "unsupported") device["supportedDeviceType"] = null;
                    if (fault == "name") device["name"] = "\u0085invalid";
                    return Task.FromResult(JsonSerializer.SerializeToElement(reply));
                default: throw new Exception("Unexpected request");
            }
        }, () => { });
        await editor.ReloadAsync();
        var error = await Assert.ThrowsAsync<HubException>(() => editor.DiscoverAlpacaAsync("http://localhost:11111/prefix/", "protected-reference"));
        Assert.DoesNotContain("must-not-escape", error.Message);
        Assert.Null(editor.LastDiscovery); Assert.Equal(HubEditorState.Uncertain, editor.State);
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.DiscoverAlpacaAsync("http://localhost:11111/prefix/"));
        Assert.Equal(1, requests); Assert.False(editor.Draft!.Dirty);
    }
    [Fact]
    public async Task NativeDiscoveryWindowDisplaysUnsupportedClassesAndStringIds()
    {
        await Wpf(async () => {
            using var server = new HubCatalogServer(); await using var host = await Host.Open();
            var window = new HubConfigurationWindow(host.Executable, host.ConfigPath, host.Selection(0, "switch").InstanceId);
            try {
                window.Height = 960;
                window.Show(); var review = Controls<Button>(window).Single(b => (string)b.Content == "Review changes");
                await UiUntil(() => review.IsEnabled);
                var tabs = Controls<TabControl>(window).Single();
                tabs.SelectedItem = tabs.Items.Cast<TabItem>().Single(tab => (string)tab.Header == "Discover devices");
                Controls<TextBox>(window).Single(box => (string?)box.Tag == "discovery-url").Text = server.Url;
                var query = Controls<Button>(window).Single(b => (string)b.Content == "Read Alpaca device catalog");
                query.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                var result = Controls<TextBox>(window).Single(box => (string?)box.Tag == "discovery-result");
                await UiUntil(() => query.IsEnabled && result.Text.Contains("camera unit 42"));
                Assert.Contains("SIMULATION", result.Text); Assert.Contains("4294967295", result.Text);
                Assert.Contains("Telescope", result.Text); Assert.Contains("not supported", result.Text);
                var selection = Controls<ComboBox>(window).Single(box => (string?)box.Tag == "discovery-selection");
                Assert.False(((ComboBoxItem)selection.Items[1]).IsEnabled);
                var add = Controls<Button>(window).Single(b => (string)b.Content == "Add selected source to draft");
                Assert.False(add.IsEnabled); selection.SelectedIndex = 0; Assert.True(add.IsEnabled);
                add.BringIntoView();
                await Capture(window, "hub-native-discovery-simulation.png");
                add.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                await UiUntil(() => query.IsEnabled);
                tabs.SelectedIndex = 0;
                Controls<Expander>(window).Single(expander => (string)expander.Header == "Sources").IsExpanded = true;
                var adopted = Controls<Expander>(window).Single(expander => (string)expander.Header == "[SIMULATION] café camera"); adopted.IsExpanded = true;
                Controls<Expander>(adopted).Single(expander => (string)expander.Header == "Backend").IsExpanded = true;
                Assert.Equal("camera unit 42", Controls<TextBox>(window).Single(box => (string?)box.Tag == "/sources/3/backend/uniqueId").Text);
                Assert.Equal(3, (await host.Command(new { op = "getConfig" })).GetProperty("sources").GetArrayLength());
                for (int i = 0; i < 3; i++) Assert.Equal(0, (await host.Status(i)).GetProperty("leaseCount").GetInt32());
                Assert.Single(server.Requests);
            } finally { window.Close(); }
        });
    }
}
