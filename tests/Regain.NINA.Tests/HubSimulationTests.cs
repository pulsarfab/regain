using System.Text.Json;
using System.Text.Json.Nodes;
using System.Windows;
using System.Windows.Controls;
using Regain.Hub;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    [Fact]
    public async Task NativeSimulationControlsPatchSharedStateAndPreserveConfigurationAndLeases()
    {
        await using var host = await Host.Open(); using var editor = await Editor(host); await editor.ReloadAsync();
        var revision = editor.Draft!.Revision;
        Guid Source(int i) => Guid.Parse(host.Config["sources"]![i]!["id"]!.GetValue<string>());
        using var device = host.Switch(); await device.Connect(CancellationToken.None);
        var controls = editor.SimulationControls(Source(0)); Assert.Equal(5, controls.Count);
        Assert.True(await editor.ReviewAsync());
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.UpdateSimulationAsync(Source(0), JsonSerializer.SerializeToElement(new { safe = true })));
        Assert.Equal(HubEditorState.Reviewed, editor.State);
        var channel = controls.Single(c => c.Path.SequenceEqual(new[] { "switchValues", "1" }));
        var update = HubEditorSession.SimulationPatch([new(channel, channel.Parse("42.2"))]);
        var result = await editor.UpdateSimulationAsync(Source(0), update);
        Assert.Equal(42, result.GetProperty("simulation").GetProperty("switchValues").GetProperty("1").GetDouble()); // backend rounds to supported step
        Assert.Equal(12, result.GetProperty("simulation").GetProperty("switchValues").GetProperty("2").GetDouble());
        Assert.Equal(HubEditorState.Editing, editor.State); Assert.True(device.Connected);
        await Eventually(async () => (await host.Status(0)).GetProperty("leaseCount").GetInt32() == 1);
        Assert.Equal("simulationUpdate", editor.DiagnosticSnapshot().GetProperty("observation").GetProperty("kind").GetString());
        Assert.Equal(2, editor.SimulationControls(Source(1)).Count);
        await editor.UpdateSimulationAsync(Source(1), JsonSerializer.SerializeToElement(new { safe = true, fault = "none" }));
        Assert.True((await host.Status(1)).GetProperty("simulation").GetProperty("safe").GetBoolean());
        var weather = editor.SimulationControls(Source(2)); Assert.Equal(15, weather.Count);
        Assert.Throws<InvalidOperationException>(() => weather.Single(c => c.Path.SequenceEqual(new[] { "weather", "pressure" })).Parse("0"));
        var temperature = weather.Single(c => c.Path.SequenceEqual(new[] { "weather", "temperature" }));
        await editor.UpdateSimulationAsync(Source(2), HubEditorSession.SimulationPatch([new(temperature, temperature.Parse("", true))]));
        Assert.Equal(JsonValueKind.Null, (await host.Status(2)).GetProperty("simulation").GetProperty("weather").GetProperty("temperature").ValueKind);
        Assert.Equal(revision, (await host.Command(new { op = "getConfig" })).GetProperty("revision").GetGuid());
        for (int i = 1; i < 3; i++) await Eventually(async () => (await host.Status(i)).GetProperty("leaseCount").GetInt32() == 0);
        device.Disconnect(); await Eventually(async () => (await host.Status(0)).GetProperty("leaseCount").GetInt32() == 0);
        var candidate = JsonNode.Parse((await host.Command(new { op = "getConfig" })).GetRawText())!;
        candidate["sources"]![0]!["label"] = "New revision of simulation";
        await host.Command(new { op = "applyConfig", expectedRevision = revision, candidate });
        var failure = await Assert.ThrowsAsync<HubException>(() => editor.UpdateSimulationAsync(Source(0), update));
        Assert.Equal("revisionConflict", failure.Remote!.Code); Assert.Equal(HubEditorState.Uncertain, editor.State);
        Assert.Equal(0, (await host.Status(0)).GetProperty("simulation").GetProperty("switchValues").GetProperty("1").GetDouble());
        await editor.ReloadAsync(); Assert.NotEqual(revision, editor.Draft.Revision);
    }
    [Theory]
    [InlineData("lost")]
    [InlineData("wrongRevision")]
    [InlineData("malformed")]
    public async Task NativeSimulationLostOrInvalidReplyRequiresReloadAndReadWithoutRepeatingChanges(string fault)
    {
        await using var host = await Host.Open(); using var attached = await Editor(host); await attached.ReloadAsync();
        var source = host.Config["sources"]![0]!["id"]!.GetValue<string>(); int writes = 0;
        using var editor = new HubEditorSession(attached.InstanceId, async (command, token) => {
            if (command.GetProperty("op").GetString() != "updateSimulation") return await host.Client.RequestAsync(command, token);
            writes++; var result = await host.Client.RequestAsync(command, token);
            if (fault == "lost") throw new HubException(HubFailure.Uncertain);
            var reply = JsonNode.Parse(result.GetRawText())!;
            if (fault == "wrongRevision") reply["configurationRevision"] = Guid.NewGuid().ToString();
            if (fault == "malformed") reply["simulation"]!["fault"] = "not-a-fault";
            return JsonSerializer.SerializeToElement(reply);
        }, () => { });
        await editor.ReloadAsync(); var patch = JsonSerializer.SerializeToElement(new { switchValues = new Dictionary<string,double> { ["1"] = 9 } });
        await Assert.ThrowsAsync<HubException>(() => editor.UpdateSimulationAsync(Guid.Parse(source),patch));
        Assert.Equal(HubEditorState.Uncertain,editor.State); Assert.Null(editor.LastSourceObservation);
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.UpdateSimulationAsync(Guid.Parse(source),patch)); Assert.Equal(1,writes);
        await editor.ReloadAsync(); Assert.Equal(9,(await editor.SourceStatusAsync(Guid.Parse(source))).GetProperty("simulation").GetProperty("switchValues").GetProperty("1").GetDouble());
        Assert.Equal(1,writes);
    }
    [Fact]
    public async Task NativeSimulationWindowChangesOnlySelectedFieldsAndResetsItsForm()
    {
        await Wpf(async () => {
            await using var host = await Host.Open();
            var window = new HubConfigurationWindow(host.Executable,host.ConfigPath,host.Selection(0,"switch").InstanceId);
            try {
                window.Show(); var review = Controls<Button>(window).Single(b => (string)b.Content == "Review changes"); await UiUntil(() => review.IsEnabled);
                Controls<TabControl>(window).Single().SelectedIndex = 4;
                var apply = Controls<Button>(window).Single(b => (string)b.Content == "Apply selected simulation changes"); Assert.False(apply.IsEnabled);
                var include = Controls<CheckBox>(window).Single(c => (string?)c.Tag == "simulation-change-switchValues-1");
                include.IsChecked = true; include.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                Controls<TextBox>(window).Single(c => (string?)c.Tag == "simulation-value-switchValues-1").Text = "17";
                await Capture(window,"hub-native-simulation-controls.png");
                apply.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(() => review.IsEnabled && !apply.IsEnabled);
                var status = await host.Status(0); Assert.Equal(17,status.GetProperty("simulation").GetProperty("switchValues").GetProperty("1").GetDouble());
                Assert.Equal(12,status.GetProperty("simulation").GetProperty("switchValues").GetProperty("2").GetDouble());
                Assert.False(Controls<CheckBox>(window).Single(c => (string?)c.Tag == "simulation-change-switchValues-1").IsChecked);
                var read = Controls<Button>(window).Single(b => (string)b.Content == "Read current simulation and reset form");
                await host.Update(0,new { switchValues = new Dictionary<string,double> { ["1"] = 27 } });
                read.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(() => read.IsEnabled);
                Assert.Equal(27,double.Parse(Controls<TextBox>(window).Single(c => (string?)c.Tag == "simulation-value-switchValues-1").Text,System.Globalization.CultureInfo.InvariantCulture));
                for (int i=0;i<3;i++) await Eventually(async () => (await host.Status(i)).GetProperty("leaseCount").GetInt32()==0);
            } finally { window.Close(); }
        });
    }
}
