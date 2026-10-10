using System.Text.Json;
using System.Text.Json.Nodes;
using Regain.Hub;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class HubTransferTests
{
    private static JsonElement Json(object value) => JsonSerializer.SerializeToElement(value);
    private static JsonElement Saved() {
        var value = JsonNode.Parse(HubDraftTests.Configuration().GetRawText())!;
        value["identities"] = new JsonObject { ["sources"] = new JsonObject(), ["outputs"] = new JsonObject(), ["channels"] = new JsonObject(), ["groups"] = new JsonArray(), ["cameraGroups"] = new JsonArray() };
        return Json(value);
    }
    private static JsonElement Document(JsonElement saved) => Json(new { formatVersion = 1, configuration = saved, credentialSources = Array.Empty<Guid>() });
    private static JsonElement Prepared(JsonElement saved, string fault = "") {
        var candidate = JsonNode.Parse(saved.GetRawText())!; candidate["outputs"]![0]!["label"] = "Imported simulation controls";
        var node = JsonNode.Parse(Json(new { configurationRevision = saved.GetProperty("revision").GetGuid(), mode = "restore", sourceInstanceId = saved.GetProperty("instanceId").GetGuid(), candidate,
            remappedIds = Array.Empty<object>(), renumberedOutputs = Array.Empty<object>(), preservedCredentials = Array.Empty<Guid>(), missingCredentials = Array.Empty<Guid>() }).GetRawText())!;
        switch (fault) {
            case "revision": node["configurationRevision"] = Guid.NewGuid(); break;
            case "instance": node["candidate"]!["instanceId"] = Guid.NewGuid(); break;
            case "history": node["candidate"]!["identities"]!["groups"]!.AsArray().Add(Guid.NewGuid()); break;
            case "mode": node["mode"] = "copy"; break;
            case "schema": node["candidate"]!["outputs"]![0]!["label"] = new JsonArray(); break;
            case "credential": node["missingCredentials"]!.AsArray().Add(Guid.NewGuid()); break;
            case "duplicate": node["missingCredentials"] = new JsonArray(JsonValue.Create(Guid.Empty), JsonValue.Create(Guid.Empty)); break;
            case "mapping": node["remappedIds"]!.AsArray().Add(new JsonObject { ["original"] = Guid.NewGuid(), ["replacement"] = Guid.NewGuid() }); break;
        }
        return Json(node);
    }
    private static HubEditorSession Editor(Func<JsonElement, Task<JsonElement>> request) {
        var saved = Saved();
        return new(saved.GetProperty("instanceId").GetGuid(), (command, _) => command.GetProperty("op").GetString() switch {
            "describeConfig" => Task.FromResult(HubDraftTests.Description()), "getConfig" => Task.FromResult(saved),
            "hostStatus" => Task.FromResult(Json(new { phase = "ready", configurationRevision = saved.GetProperty("revision").GetGuid() })),
            "validateConfig" => Task.FromResult(Json(new { valid = true, errors = Array.Empty<object>() })), _ => request(command)
        }, () => { });
    }
    [Fact]
    public async Task ImportReplacesOnlyTheDraftAndKeepsTheSavedBaseline() {
        var saved = Saved(); var calls = 0;
        using var editor = Editor(command => {
            calls++; Assert.Equal("prepareImport", command.GetProperty("op").GetString()); Assert.Equal("restore", command.GetProperty("mode").GetString());
            Assert.Equal(saved.GetProperty("revision").GetGuid(), command.GetProperty("expectedRevision").GetGuid()); return Task.FromResult(Prepared(saved));
        });
        await editor.ReloadAsync(); Assert.True(await editor.ReviewAsync());
        var result = await editor.PrepareImportAsync(Document(saved).GetRawText(), "restore");
        Assert.Equal(HubEditorState.Editing, editor.State); Assert.True(editor.Draft!.Dirty);
        Assert.Equal("Imported simulation controls", editor.Draft.Candidate.GetProperty("outputs")[0].GetProperty("label").GetString());
        Assert.Equal(saved.GetRawText(), editor.SavedConfiguration!.Value.GetRawText());
        Assert.True(editor.Draft.Field("/outputs/0/number").ReadOnly); Assert.Contains("Saved configuration is unchanged", HubEditorSession.ImportSummary(result));
        Assert.Equal(1, calls); await editor.ReloadAsync(); Assert.Null(editor.LastImport);
    }
    [Theory]
    [InlineData("revision")] [InlineData("instance")] [InlineData("history")] [InlineData("mode")]
    [InlineData("schema")] [InlineData("credential")] [InlineData("duplicate")] [InlineData("mapping")]
    public async Task MalformedResponsesNeverReplaceTheExistingDraft(string fault) {
        using var editor = Editor(_ => Task.FromResult(Prepared(Saved(), fault))); await editor.ReloadAsync();
        editor.Draft!.SetValue("/outputs/0/label", Json("Local change")); editor.Changed(); Assert.True(await editor.ReviewAsync());
        var before = editor.Draft.Candidate.GetRawText();
        await Assert.ThrowsAsync<HubException>(() => editor.PrepareImportAsync(Document(Saved()).GetRawText(), "restore"));
        Assert.Equal(before, editor.Draft.Candidate.GetRawText()); Assert.Null(editor.LastImport); Assert.Equal(HubEditorState.Uncertain, editor.State);
    }
    [Fact]
    public async Task ExportUsesSavedConfigurationAndDoesNotRevokeAReview() {
        using var editor = Editor(command => {
            Assert.Equal("exportConfig", command.GetProperty("op").GetString()); return Task.FromResult(Document(Saved()));
        });
        await editor.ReloadAsync(); editor.Draft!.SetValue("/outputs/0/label", Json("Unsaved")); editor.Changed(); Assert.True(await editor.ReviewAsync());
        Assert.DoesNotContain("Unsaved", await editor.ExportConfigurationAsync()); Assert.Equal(HubEditorState.Reviewed, editor.State);
    }
    [Fact]
    public async Task ConcurrentDraftChangesAreNotOverwrittenByAnImportReply() {
        var reply = new TaskCompletionSource<JsonElement>(TaskCreationOptions.RunContinuationsAsynchronously);
        using var editor = Editor(_ => reply.Task); await editor.ReloadAsync();
        var preparing = editor.PrepareImportAsync(Document(Saved()).GetRawText(), "restore");
        editor.Draft!.SetValue("/outputs/0/label", Json("Later local change")); editor.Changed(); reply.SetResult(Prepared(Saved()));
        await Assert.ThrowsAsync<InvalidOperationException>(() => preparing);
        Assert.Equal("Later local change", editor.Draft.Candidate.GetProperty("outputs")[0].GetProperty("label").GetString()); Assert.Null(editor.LastImport);
    }
    [Fact]
    public async Task FileReadsAreBoundedAndInvalidModesDoNotContactTheHost() {
        var path = Path.GetTempFileName(); try {
            await File.WriteAllBytesAsync(path, new byte[100]);
            await Assert.ThrowsAsync<InvalidOperationException>(() => HubConfigurationWindow.ReadConfigurationFile(path, 99, default));
            Assert.Equal(100, (await HubConfigurationWindow.ReadConfigurationFile(path, 100, default)).Length);
        } finally { File.Delete(path); }
        using var editor = Editor(_ => throw new Exception("Unexpected I/O")); await editor.ReloadAsync(); Assert.True(await editor.ReviewAsync());
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.PrepareImportAsync("{}", "merge"));
        Assert.Equal(HubEditorState.Reviewed, editor.State);
    }
}

public sealed partial class HubNativeTests
{
    [Fact]
    public async Task ProductionConfigurationTransferPreparesReviewsAndAppliesWithoutLeases() {
        await using var host = await Host.Open(); using var editor = await Editor(host); await editor.ReloadAsync();
        var file = await editor.ExportConfigurationAsync(); var document = JsonNode.Parse(file)!;
        document["configuration"]!["outputs"]![0]!["label"] = "Imported controls [SIMULATION]";
        var saved = editor.SavedConfiguration!.Value.GetRawText();
        await editor.PrepareImportAsync(document.ToJsonString(), "restore"); Assert.Equal(saved, editor.SavedConfiguration.Value.GetRawText());
        for (int i = 0; i < 3; i++) Assert.Equal(0, (await host.Status(i)).GetProperty("leaseCount").GetInt32());
        Assert.True(await editor.ReviewAsync()); await editor.ApplyAsync(); await editor.ReloadAsync();
        Assert.Equal("Imported controls [SIMULATION]", editor.SavedConfiguration.Value.GetProperty("outputs")[0].GetProperty("label").GetString());
        var copied = await editor.PrepareImportAsync(file, "copy"); Assert.NotEmpty(copied.GetProperty("remappedIds").EnumerateArray());
        Assert.Equal(3, copied.GetProperty("renumberedOutputs").GetArrayLength()); Assert.True(await editor.ReviewAsync());
    }
    [Fact]
    public async Task NativeTransferPanelShowsAProductionImportedDraft() {
        await Wpf(async () => {
            await using var host = await Host.Open(); var saved = await host.Command(new { op = "getConfig" });
            var document = await host.Command(new { op = "exportConfig", expectedRevision = saved.GetProperty("revision").GetGuid() });
            var window = new HubConfigurationWindow(host.Executable, host.ConfigPath, host.Selection(0, "switch").InstanceId);
            try {
                window.Height = 820; window.Show();
                await UiUntil(() => Controls<System.Windows.Controls.Button>(window).Single(b => (string)b.Content == "Review changes").IsEnabled);
                var tabs = Controls<System.Windows.Controls.TabControl>(window).Single(); tabs.SelectedItem = tabs.Items.Cast<System.Windows.Controls.TabItem>().Single(t => (string)t.Header == "Import / export");
                var mode = Controls<System.Windows.Controls.ComboBox>(window).Single(box => (string?)box.Tag == "configuration-import-mode");
                mode.SelectedItem = mode.Items.Cast<System.Windows.Controls.ComboBoxItem>().Single(item => (string)item.Tag == "copy");
                await window.ImportConfigurationDraft(document.GetRawText(), (string)((System.Windows.Controls.ComboBoxItem)mode.SelectedItem).Tag);
                var summary = Controls<System.Windows.Controls.TextBox>(window).Single(t => (string?)t.Tag == "configuration-import-result");
                Assert.Contains("Changed device numbers: 3", summary.Text); Assert.Contains("Saved configuration is unchanged", summary.Text);
                await Capture(window, "hub-native-configuration-transfer-simulation.png");
                Assert.Equal(saved.GetRawText(), (await host.Command(new { op = "getConfig" })).GetRawText());
                for (int i = 0; i < 3; i++) Assert.Equal(0, (await host.Status(i)).GetProperty("leaseCount").GetInt32());
            } finally { window.Close(); }
        });
    }
}
