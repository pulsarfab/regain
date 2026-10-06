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
    public async Task NativeInspectionPagesSavedSourcesAndPreservesOtherLeases()
    {
        await using var host = await Host.Open(); using var editor = await Editor(host); await editor.ReloadAsync();
        var revision = editor.Draft!.Revision;
        Guid Source(int i) => Guid.Parse(host.Config["sources"]![i]!["id"]!.GetValue<string>());
        Assert.True(await editor.ReviewAsync());
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.InspectSourceAsync(Source(0), 0, 0));
        Assert.Equal(HubEditorState.Reviewed, editor.State);
        var first = await editor.InspectSourceAsync(Source(0), 0, 2);
        Assert.True(first.GetProperty("simulation").GetBoolean());
        Assert.Equal(2, first.GetProperty("capabilities").GetProperty("channels").GetArrayLength());
        Assert.Equal(2, first.GetProperty("capabilities").GetProperty("nextStart").GetInt32());
        Assert.Equal(HubEditorState.Editing, editor.State);
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.ApplyAsync());
        var last = await editor.InspectSourceAsync(Source(0), 2, 2);
        Assert.Single(last.GetProperty("capabilities").GetProperty("channels").EnumerateArray());
        Assert.Equal(JsonValueKind.Null, last.GetProperty("capabilities").GetProperty("nextStart").ValueKind);
        await Eventually(async () => (await host.Status(0)).GetProperty("leaseCount").GetInt32() == 0);
        using var device = host.Switch(); await device.Connect(CancellationToken.None);
        await editor.InspectSourceAsync(Source(0), 0, 2);
        await Eventually(async () => (await host.Status(0)).GetProperty("leaseCount").GetInt32() == 1);
        Assert.True(device.Connected);
        Assert.Equal("safety", (await editor.InspectSourceAsync(Source(1), 0, 2)).GetProperty("capabilities").GetProperty("kind").GetString());
        Assert.Equal("weather", (await editor.InspectSourceAsync(Source(2), 0, 2)).GetProperty("capabilities").GetProperty("kind").GetString());
        var export = editor.DiagnosticSnapshot();
        Assert.Equal(revision, export.GetProperty("savedRevision").GetGuid());
        Assert.Equal(Source(2), export.GetProperty("observation").GetProperty("result").GetProperty("source").GetGuid());
        Assert.False(export.TryGetProperty("configuration", out _)); Assert.DoesNotContain("credential", export.GetRawText(), StringComparison.OrdinalIgnoreCase);
        Assert.Equal(revision, (await host.Command(new { op = "getConfig" })).GetProperty("revision").GetGuid());
        await editor.ReloadAsync(); Assert.Equal(JsonValueKind.Null, editor.DiagnosticSnapshot().GetProperty("observation").ValueKind);
    }
    [Theory]
    [InlineData("lost")]
    [InlineData("wrongRevision")]
    [InlineData("wrongSource")]
    [InlineData("secretMember")]
    [InlineData("wrongCursor")]
    public async Task NativeInspectionRejectsLostOrObsoleteRepliesWithoutReplay(string fault)
    {
        var saved = HubDraftTests.Configuration(); int requests = 0;
        var description = JsonNode.Parse(HubDraftTests.Description().GetRawText())!.AsObject();
        description["capabilityInspection"] = JsonSerializer.SerializeToNode(new {
            purpose = "setupOnly", opensSource = true, writesEquipment = false, deadlineSeconds = 20,
            parameters = new { start = new { type = "integer", label = "Custom start", minimum = 3, maximum = 10, @default = 3 },
                limit = new { type = "integer", label = "Custom limit", minimum = 2, maximum = 3, @default = 2 } }
        });
        var source = saved.GetProperty("sources")[0].GetProperty("id").GetGuid();
        using var editor = new HubEditorSession(saved.GetProperty("instanceId").GetGuid(), (command, _) => {
            switch (command.GetProperty("op").GetString()) {
                case "describeConfig": return Task.FromResult(JsonSerializer.SerializeToElement(description));
                case "getConfig": return Task.FromResult(saved);
                case "hostStatus": return Task.FromResult(JsonSerializer.SerializeToElement(new { phase = "ready", configurationRevision = saved.GetProperty("revision").GetGuid() }));
                case "inspectSource":
                    requests++;
                    Assert.Equal(3, command.GetProperty("start").GetInt32()); Assert.Equal(2, command.GetProperty("limit").GetInt32());
                    if (fault == "lost") throw new HubException(HubFailure.Disconnected);
                    var reply = JsonSerializer.SerializeToNode(new { purpose = "setupOnly", source = fault == "wrongSource" ? Guid.NewGuid() : source,
                        configurationRevision = fault == "wrongRevision" ? Guid.NewGuid() : saved.GetProperty("revision").GetGuid(), generation = Guid.NewGuid(),
                        deviceType = "switch", connection = (object?)null, simulation = true, startedSeconds = 1, completedSeconds = 2,
                        capabilities = new { kind = "switch" } })!.AsObject();
                    if (fault == "secretMember") reply["authorization"] = "should not escape";
                    if (fault == "wrongCursor") reply["capabilities"]!["nextStart"] = 1;
                    return Task.FromResult(JsonSerializer.SerializeToElement(reply));
                default: throw new Exception("Unexpected request");
            }
        }, () => { });
        await editor.ReloadAsync();
        Assert.Throws<InvalidOperationException>(() => editor.InspectionParameter("start", "2"));
        Assert.Throws<InvalidOperationException>(() => editor.InspectionParameter("limit", "4"));
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.InspectSourceAsync(Guid.NewGuid(), 3, 2));
        Assert.Equal(0, requests);
        var error = await Assert.ThrowsAsync<HubException>(() => editor.InspectSourceAsync(source, 3, 2));
        Assert.DoesNotContain("should not escape", error.Message);
        Assert.Equal(HubEditorState.Uncertain, editor.State); Assert.Null(editor.LastSourceObservation);
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.InspectSourceAsync(source, 3, 2));
        Assert.Equal(1, requests);
        await editor.ReloadAsync(); Assert.Equal(1, requests); Assert.Equal(HubEditorState.Editing, editor.State);
    }
    [Fact]
    public async Task NativeInspectionCancellationDisposesOnlyItsSessionAndCannotRestoreAReview()
    {
        await using var host = await Host.Open(); using var attached = await Editor(host); await attached.ReloadAsync();
        var saved = attached.SavedConfiguration!.Value; var description = attached.Description!.Value;
        var entered = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously); int probes = 0, closed = 0;
        using var editor = new HubEditorSession(attached.InstanceId, async (command, token) => {
            switch (command.GetProperty("op").GetString()) {
                case "describeConfig": return description;
                case "getConfig": return saved;
                case "hostStatus": return attached.HostStatus!.Value;
                case "validateConfig": return JsonSerializer.SerializeToElement(new { valid = true, errors = Array.Empty<object>() });
                case "inspectSource": probes++; entered.TrySetResult(true); await Task.Delay(Timeout.Infinite, token); throw new Exception("Unreachable");
                default: throw new Exception("Unexpected request");
            }
        }, () => closed++);
        await editor.ReloadAsync(); Assert.True(await editor.ReviewAsync());
        using var cancellation = new CancellationTokenSource();
        var pending = editor.InspectSourceAsync(saved.GetProperty("sources")[0].GetProperty("id").GetGuid(), 0, 2, cancellation.Token);
        await entered.Task.WaitAsync(TimeSpan.FromSeconds(5)); cancellation.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => pending);
        Assert.Equal(HubEditorState.Uncertain, editor.State); Assert.Null(editor.LastSourceObservation);
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.ApplyAsync()); Assert.Equal(1, probes);
        editor.Dispose(); Assert.Equal(1, closed); Assert.Equal(HubEditorState.Disposed, editor.State);
        Assert.Equal(saved.GetProperty("revision").GetGuid(), (await host.Command(new { op = "getConfig" })).GetProperty("revision").GetGuid());
        for (int i = 0; i < 3; i++) Assert.Equal(0, (await host.Status(i)).GetProperty("leaseCount").GetInt32());
    }
    [Fact]
    public async Task NativeInspectionWindowUsesHostPagingAndShowsSimulation()
    {
        await Wpf(async () => {
            await using var host = await Host.Open();
            var window = new HubConfigurationWindow(host.Executable, host.ConfigPath, host.Selection(0, "switch").InstanceId);
            try {
                window.Show(); var review = Controls<Button>(window).Single(b => (string)b.Content == "Review changes");
                await UiUntil(() => review.IsEnabled);
                Controls<TabControl>(window).Single().SelectedIndex = 2;
                var limit = Controls<TextBox>(window).Single(t => (string?)t.Tag == "inspection-limit"); limit.Text = "2";
                var inspect = Controls<Button>(window).Single(b => (string)b.Content == "Inspect saved source");
                var next = Controls<Button>(window).Single(b => (string)b.Content == "Inspect next channels");
                review.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                var apply = Controls<Button>(window).Single(b => (string)b.Content == "Apply reviewed configuration");
                await UiUntil(() => apply.IsEnabled); Controls<TabControl>(window).Single().SelectedIndex = 2;
                limit.Text = "0"; inspect.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(() => inspect.IsEnabled);
                Assert.True(apply.IsEnabled);
                Assert.NotEmpty(Controls<TextBox>(window).Single(t => t.IsReadOnly && t.FontFamily.Source == "Consolas").Text);
                limit.Text = "2";
                inspect.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(() => next.IsEnabled);
                Assert.False(apply.IsEnabled);
                var result = Controls<TextBox>(window).Single(t => (string?)t.Tag == "source-result");
                using (var json = JsonDocument.Parse(result.Text)) Assert.True(json.RootElement.GetProperty("simulation").GetBoolean());
                await Capture(window, "hub-native-inspection-simulation.png");
                next.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(() => inspect.IsEnabled);
                Assert.False(next.IsEnabled); Assert.Equal("2", Controls<TextBox>(window).Single(t => (string?)t.Tag == "inspection-start").Text);
                var selected = Controls<ComboBox>(window).Single(c => (string?)c.Tag == "inspection-source"); selected.SelectedIndex = 1;
                Assert.Empty(result.Text); Assert.False(next.IsEnabled);
                for (int i = 0; i < 3; i++) await Eventually(async () => (await host.Status(i)).GetProperty("leaseCount").GetInt32() == 0);
            } finally { window.Close(); }
        });
    }
}
