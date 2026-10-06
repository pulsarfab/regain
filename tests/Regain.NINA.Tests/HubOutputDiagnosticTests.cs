using System.Text.Json;
using System.Text.Json.Nodes;
using System.Windows;
using System.Windows.Controls;
using Regain.Hub;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    [Theory]
    [InlineData("focuser", "fc3", "00:00:00:00:00:03", 4)]
    [InlineData("rotator", "caa", "0102030405060708", 3)]
    public async Task TypedAccessoryDiagnosticsUseHostFieldsAndRejectIdentityTypeAndRangeFaults(string deviceType, string nativeDevice, string identity, int positionIndex)
    {
        await using var host = await Host.Open(); using var editor = await Editor(host); await editor.ReloadAsync();
        var saved = JsonNode.Parse(editor.SavedConfiguration!.Value.GetRawText())!.AsObject();
        var output = saved["outputs"]![0]!.DeepClone().AsObject(); var source = saved["sources"]![0]!["id"]!.GetValue<string>();
        var sourceConfig = saved["sources"]![0]!.DeepClone().AsObject();
        sourceConfig["backend"] = new JsonObject { ["kind"] = "native", ["device"] = nativeDevice, ["identity"] = identity };
        saved["sources"] = new JsonArray(sourceConfig);
        output["device"] = new JsonObject { ["kind"] = "proxy", ["deviceType"] = deviceType, ["source"] = source };
        saved["outputs"] = new JsonArray(output); saved["identities"] = new JsonObject { ["outputs"] = new JsonObject(), ["channels"] = new JsonObject() };
        var original = await editor.OutputStatusAsync(Guid.Parse(output["id"]!.GetValue<string>()), 0, 1);
        var health = JsonNode.Parse(original.GetProperty("diagnostics").GetProperty("channels")[0].GetProperty("health").GetRawText())!;
        health["transportConnected"] = true; health["leaseCount"] = 1;
        var fields = editor.OutputDiagnosticDescription.GetProperty(deviceType + "Properties");
        JsonObject Reply(int start, int limit) {
            var properties = new JsonArray(); int end = Math.Min(start + limit, fields.GetArrayLength());
            for (int i = start; i < end; i++) {
                var field = fields[i]; var type = field.GetProperty("valueType").GetString();
                properties.Add(new JsonObject { ["property"] = field.GetProperty("property").GetString(),
                    ["sample"] = new JsonObject { ["state"] = "available", ["reading"] = new JsonObject {
                        ["value"] = new JsonObject { ["type"] = type, ["value"] = type == "boolean" ? JsonValue.Create(true) : JsonValue.Create(1) },
                        ["ageSeconds"] = 0.5, ["source"] = source, ["generation"] = health["generation"]!.DeepClone(),
                        ["sequence"] = health["sequence"]!.DeepClone(), ["revision"] = saved["revision"]!.DeepClone() } } });
            }
            return new JsonObject { ["purpose"] = "cachedDiagnostics", ["output"] = output["id"]!.DeepClone(),
                ["configurationRevision"] = saved["revision"]!.DeepClone(), ["observedSeconds"] = 1, ["deviceType"] = deviceType,
                ["simulated"] = true, ["start"] = start, ["limit"] = limit, ["total"] = fields.GetArrayLength(),
                ["nextStart"] = end < fields.GetArrayLength() ? JsonValue.Create(end) : null,
                ["diagnostics"] = new JsonObject { ["kind"] = deviceType, ["health"] = health.DeepClone(), ["properties"] = properties } };
        }
        JsonElement Element(JsonNode node) => JsonSerializer.SerializeToElement(node);
        var first = Reply(0, 4); HubDiagnosticContract.Reply(editor.OutputDiagnosticDescription, Element(saved), Element(output), Element(first), 0, 4);
        var last = Reply(4, 32); HubDiagnosticContract.Reply(editor.OutputDiagnosticDescription, Element(saved), Element(output), Element(last), 4, 32);
        var summary = HubConfigurationWindow.OutputDiagnosticSummary(Element(Reply(positionIndex, 32))); Assert.Contains("position: 1", summary); Assert.Contains("age 0.5 s", summary);
        foreach (var fault in new[] { "property", "source", "generation", "sequence", "type", "minimum", "extra", "age" }) {
            var reply = Reply(positionIndex, 1); var item = reply["diagnostics"]!["properties"]![0]!; var reading = item["sample"]!["reading"]!;
            if (fault == "property") item["property"] = "isMoving";
            if (fault == "source") reading["source"] = Guid.NewGuid().ToString();
            if (fault == "generation") reading["generation"] = Guid.NewGuid().ToString();
            if (fault == "sequence") reading["sequence"] = 1;
            if (fault == "type") reading["value"]!["type"] = deviceType == "focuser" ? "number" : "integer";
            if (fault == "minimum") reading["value"]!["value"] = -1;
            if (fault == "extra") reading["authorization"] = "PRIVATE_FORBIDDEN_REPLY";
            if (fault == "age") reading["ageSeconds"] = -1;
            Assert.Throws<HubException>(() => HubDiagnosticContract.Reply(editor.OutputDiagnosticDescription, Element(saved), Element(output), Element(reply), positionIndex, 1));
        }
        if (deviceType == "rotator") {
            foreach (var (index, value) in new[] { (3, 360.0), (5, 0.0), (5, (double)float.MaxValue * 2) }) {
                var reply = Reply(index, 1); reply["diagnostics"]!["properties"]![0]!["sample"]!["reading"]!["value"]!["value"] = value;
                Assert.Throws<HubException>(() => HubDiagnosticContract.Reply(editor.OutputDiagnosticDescription, Element(saved), Element(output), Element(reply), index, 1));
            }
        }
    }
    [Fact]
    public async Task WheelDiagnosticsPreserveMetadataAndRejectMalformedArraysAndEpochs()
    {
        // The host uses its ordinary scalar fixture only to supply the current
        // contract and health shape. This native wheel configuration is never
        // applied or connected; all wheel responses below are private fixtures.
        await using var host = await Host.Open(); using var editor = await Editor(host); await editor.ReloadAsync();
        var saved = JsonNode.Parse(editor.SavedConfiguration!.Value.GetRawText())!.AsObject();
        var output = saved["outputs"]![0]!.DeepClone().AsObject(); var sourceConfig = saved["sources"]![0]!.DeepClone().AsObject();
        var source = sourceConfig["id"]!.GetValue<string>();
        sourceConfig["backend"] = new JsonObject { ["kind"] = "native", ["device"] = "efw", ["identity"] = "0102030405060708" };
        saved["sources"] = new JsonArray(sourceConfig);
        output["device"] = new JsonObject { ["kind"] = "proxy", ["deviceType"] = "filterwheel", ["source"] = source };
        saved["outputs"] = new JsonArray(output); saved["identities"] = new JsonObject { ["outputs"] = new JsonObject(), ["channels"] = new JsonObject() };
        var original = await editor.OutputStatusAsync(Guid.Parse(output["id"]!.GetValue<string>()), 0, 1);
        var health = JsonNode.Parse(original.GetProperty("diagnostics").GetProperty("channels")[0].GetProperty("health").GetRawText())!;
        health["transportConnected"] = true; health["leaseCount"] = 1;
        var fields = editor.OutputDiagnosticDescription.GetProperty("filterwheelProperties"); Assert.Equal(3, fields.GetArrayLength());
        JsonObject Reply(int start, int limit) {
            var properties = new JsonArray(); int end = Math.Min(start + limit, fields.GetArrayLength());
            for (int i = start; i < end; i++) {
                var field = fields[i]; JsonNode value = i switch { 0 => new JsonArray("L", "Hα", ""), 1 => new JsonArray(-12, 0, 17), _ => JsonValue.Create(-1)! };
                properties.Add(new JsonObject { ["property"] = field.GetProperty("property").GetString(),
                    ["sample"] = new JsonObject { ["state"] = "available", ["reading"] = new JsonObject {
                        ["value"] = new JsonObject { ["type"] = field.GetProperty("valueType").GetString(), ["value"] = value },
                        ["ageSeconds"] = 5, ["source"] = source, ["generation"] = health["generation"]!.DeepClone(),
                        ["sequence"] = health["sequence"]!.DeepClone(), ["revision"] = saved["revision"]!.DeepClone() } } });
            }
            return new JsonObject { ["purpose"] = "cachedDiagnostics", ["output"] = output["id"]!.DeepClone(),
                ["configurationRevision"] = saved["revision"]!.DeepClone(), ["observedSeconds"] = 6, ["deviceType"] = "filterwheel",
                ["simulated"] = true, ["start"] = start, ["limit"] = limit, ["total"] = fields.GetArrayLength(),
                ["nextStart"] = end < fields.GetArrayLength() ? JsonValue.Create(end) : null,
                ["diagnostics"] = new JsonObject { ["kind"] = "filterwheel", ["health"] = health.DeepClone(), ["properties"] = properties } };
        }
        JsonElement Element(JsonNode node) => JsonSerializer.SerializeToElement(node);
        void Validate(JsonObject reply, int start, int limit) => HubDiagnosticContract.Reply(editor.OutputDiagnosticDescription, Element(saved), Element(output), Element(reply), start, limit);
        Validate(Reply(0, 2), 0, 2); Validate(Reply(2, 32), 2, 32); Validate(Reply(3, 1), 3, 1);
        var summary = HubConfigurationWindow.OutputDiagnosticSummary(Element(Reply(0, 3)));
        Assert.Contains(JsonSerializer.Serialize(new[] { "L", "Hα", "" }), summary);
        Assert.Contains("[-12,0,17]", summary); Assert.Contains("position: -1", summary);
        foreach (var (index, value) in new (int, string)[] {
            (0,"[]"),(0,"[1]"),(0,"[[\"L\"]]"),(0,JsonSerializer.Serialize(Enumerable.Repeat("L",1025))),
            (1,"[]"),(1,"[1,2]"),(1,"[0,1.5]"),(1,"[0,2147483648]"),(1,"[0,-2147483649]"),
            (1,"[0,\"1\"]"),(1,"[0,[1]]"),(1,JsonSerializer.Serialize(Enumerable.Repeat(0,1025))),
            (2,"-2"),(2,"1024"),(2,"0.5") }) {
            var reply = Reply(index,1); reply["diagnostics"]!["properties"]![0]!["sample"]!["reading"]!["value"]!["value"] = JsonNode.Parse(value);
            Assert.Throws<HubException>(() => Validate(reply,index,1));
        }
        foreach (var fault in new[] { "property", "source", "generation", "sequence", "type", "extra", "age" }) {
            var reply = Reply(0,1); var item = reply["diagnostics"]!["properties"]![0]!; var reading = item["sample"]!["reading"]!;
            if (fault == "property") item["property"] = "position";
            if (fault == "source") reading["source"] = Guid.NewGuid().ToString();
            if (fault == "generation") reading["generation"] = Guid.NewGuid().ToString();
            if (fault == "sequence") reading["sequence"] = health["sequence"]!.GetValue<ulong>() + 1;
            if (fault == "type") reading["value"] = new JsonObject { ["type"] = "integer", ["value"] = 0 };
            if (fault == "extra") reading["authorization"] = "PRIVATE_FORBIDDEN_REPLY";
            if (fault == "age") reading["ageSeconds"] = -1;
            Assert.Throws<HubException>(() => Validate(reply,0,1));
        }
        Assert.Equal(0, (await host.Status(0)).GetProperty("leaseCount").GetInt32());
    }
    [Fact]
    public void DiagnosticSchemaReferencesRetainSiblingConstraintsAndDecodedStrings()
    {
        using var schema = JsonDocument.Parse("{\"$defs\":{\"value\":{\"type\":\"number\",\"minimum\":2}},\"$ref\":\"#/$defs/value\",\"maximum\":3}");
        HubDiagnosticContract.Schema(schema.RootElement, JsonSerializer.SerializeToElement(2));
        Assert.Throws<HubException>(() => HubDiagnosticContract.Schema(schema.RootElement, JsonSerializer.SerializeToElement(1)));
        Assert.Throws<HubException>(() => HubDiagnosticContract.Schema(schema.RootElement, JsonSerializer.SerializeToElement(4)));
        using var escaped = JsonDocument.Parse("\"\\u00b0C\"");
        using var plain = JsonDocument.Parse("\"°C\"");
        Assert.True(HubDiagnosticContract.Equal(escaped.RootElement, plain.RootElement));
    }
    [Fact]
    public async Task CachedOutputEditorPreservesReviewSavedIdentityAndSiblingLeases()
    {
        await using var host = await Host.Open(); using var editor = await Editor(host); await editor.ReloadAsync();
        var saved = editor.SavedConfiguration!.Value; Guid Output(int i) => saved.GetProperty("outputs")[i].GetProperty("id").GetGuid();
        Assert.True(await editor.ReviewAsync());
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.OutputStatusAsync(Guid.NewGuid(), 0, 1));
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.OutputStatusAsync(Output(0), 0, 0));
        Assert.Equal(HubEditorState.Reviewed, editor.State);
        var first = await editor.OutputStatusAsync(Output(0), 0, 1); Assert.Equal(1, first.GetProperty("nextStart").GetInt32());
        Assert.Equal(HubEditorState.Reviewed, editor.State);
        var second = await editor.OutputStatusAsync(Output(0), 1, 32); Assert.Equal(2, second.GetProperty("diagnostics").GetProperty("channels").GetArrayLength());
        var safety = await editor.OutputStatusAsync(Output(1), 0, 32);
        Assert.False(safety.GetProperty("diagnostics").GetProperty("isSafe").GetBoolean()); Assert.Contains("raw unknown", HubConfigurationWindow.OutputDiagnosticSummary(safety));
        Assert.Contains("polling idle", HubConfigurationWindow.OutputDiagnosticSummary(safety));
        Assert.Equal("idle", safety.GetProperty("diagnostics").GetProperty("members")[0].GetProperty("health").GetProperty("polling").GetProperty("phase").GetString());
        await editor.OutputStatusAsync(Output(2), 0, 32);
        for (int i = 0; i < 3; i++) Assert.Equal(0, (await host.Status(i)).GetProperty("leaseCount").GetInt32());
        using var device = host.Switch(); await device.Connect(CancellationToken.None);
        var observed = await editor.OutputStatusAsync(Output(0), 0, 32);
        Assert.Equal(1, observed.GetProperty("diagnostics").GetProperty("channels")[0].GetProperty("health").GetProperty("leaseCount").GetInt32());
        Assert.NotEqual("idle", observed.GetProperty("diagnostics").GetProperty("channels")[0].GetProperty("health").GetProperty("polling").GetProperty("phase").GetString());
        Assert.True(device.Connected); Assert.Equal(HubEditorState.Reviewed, editor.State);
        var export = editor.DiagnosticSnapshot(); Assert.Equal("cachedOutputHealth", export.GetProperty("outputObservation").GetProperty("kind").GetString());
        Assert.Equal(saved.GetProperty("revision").GetGuid(), export.GetProperty("outputObservation").GetProperty("configurationRevision").GetGuid());
        Assert.DoesNotContain("credential", export.GetRawText(), StringComparison.OrdinalIgnoreCase); Assert.False(export.TryGetProperty("configuration", out _));
        await editor.ReloadAsync(); Assert.Null(editor.LastOutputObservation); Assert.Equal(JsonValueKind.Null, editor.DiagnosticSnapshot().GetProperty("outputObservation").ValueKind);
    }
    [Theory]
    [InlineData("lost")]
    [InlineData("revision")]
    [InlineData("output")]
    [InlineData("cursor")]
    [InlineData("channel")]
    [InlineData("secretRoot")]
    [InlineData("secretHealth")]
    [InlineData("numberType")]
    [InlineData("missingError")]
    [InlineData("missingPolling")]
    [InlineData("negativeWait")]
    [InlineData("impossibleWait")]
    public async Task CachedOutputRepliesAreStrictAndCannotAuthorizeReplayOrAnExport(string fault)
    {
        await using var host = await Host.Open(); using var original = await Editor(host); await original.ReloadAsync();
        var saved = original.SavedConfiguration!.Value; var description = original.Description!.Value;
        var output = saved.GetProperty("outputs")[0].GetProperty("id").GetGuid(); int requests = 0;
        using var editor = new HubEditorSession(original.InstanceId, async (command, token) => {
            switch (command.GetProperty("op").GetString()) {
                case "describeConfig": return description;
                case "getConfig": return saved;
                case "hostStatus": return original.HostStatus!.Value;
                case "validateConfig": return JsonSerializer.SerializeToElement(new { valid = true, errors = Array.Empty<object>() });
                case "outputStatus":
                    requests++; Assert.Equal(saved.GetProperty("revision").GetGuid(), command.GetProperty("expectedRevision").GetGuid());
                    if (fault == "lost") throw new HubException(HubFailure.Disconnected);
                    var reply = JsonNode.Parse((await host.Command(new { op = "outputStatus", output, expectedRevision = saved.GetProperty("revision").GetGuid(), start = 0, limit = 1 })).GetRawText())!;
                    if (fault == "revision") reply["configurationRevision"] = Guid.NewGuid().ToString();
                    if (fault == "output") reply["output"] = Guid.NewGuid().ToString();
                    if (fault == "cursor") reply["nextStart"] = 0;
                    if (fault == "channel") reply["diagnostics"]!["channels"]![0]!["id"] = Guid.NewGuid().ToString();
                    if (fault == "secretRoot") reply["authorization"] = "PRIVATE_FORBIDDEN_REPLY";
                    if (fault == "secretHealth") reply["diagnostics"]!["channels"]![0]!["health"]!["authorization"] = "PRIVATE_FORBIDDEN_REPLY";
                    if (fault == "numberType") reply["diagnostics"]!["channels"]![0]!["minimum"] = "0";
                    if (fault == "missingError") reply["diagnostics"]!["channels"]![0]!["health"]!.AsObject().Remove("error");
                    if (fault == "missingPolling") reply["diagnostics"]!["channels"]![0]!["health"]!.AsObject().Remove("polling");
                    if (fault == "negativeWait") reply["diagnostics"]!["channels"]![0]!["health"]!["polling"]!["nextPollAfterSeconds"] = -1;
                    if (fault == "impossibleWait") reply["diagnostics"]!["channels"]![0]!["health"]!["polling"]!["nextPollAfterSeconds"] = 10;
                    return JsonSerializer.SerializeToElement(reply);
                default: throw new Exception("Unexpected request");
            }
        }, () => { });
        await editor.ReloadAsync(); Assert.True(await editor.ReviewAsync());
        var error = await Assert.ThrowsAsync<HubException>(() => editor.OutputStatusAsync(output, 0, 1));
        Assert.DoesNotContain("PRIVATE_FORBIDDEN_REPLY", error.Message); Assert.Equal(HubEditorState.Uncertain, editor.State); Assert.Null(editor.LastOutputObservation);
        Assert.DoesNotContain("PRIVATE_FORBIDDEN_REPLY", editor.DiagnosticSnapshot().GetRawText());
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.OutputStatusAsync(output, 0, 1)); Assert.Equal(1, requests);
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.ApplyAsync());
        await editor.ReloadAsync(); Assert.Equal(1, requests); Assert.Equal(HubEditorState.Editing, editor.State);
    }
    [Fact]
    public async Task CachedOutputCancellationCannotPreserveAReviewOrDisposeAnotherClient()
    {
        await using var host = await Host.Open(); using var original = await Editor(host); await original.ReloadAsync();
        var saved = original.SavedConfiguration!.Value; var entered = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously); int closed = 0;
        using var editor = new HubEditorSession(original.InstanceId, async (command, token) => {
            switch (command.GetProperty("op").GetString()) {
                case "describeConfig": return original.Description!.Value;
                case "getConfig": return saved;
                case "hostStatus": return original.HostStatus!.Value;
                case "validateConfig": return JsonSerializer.SerializeToElement(new { valid = true, errors = Array.Empty<object>() });
                case "outputStatus": entered.SetResult(true); await Task.Delay(Timeout.Infinite, token); throw new Exception("Unreachable");
                default: throw new Exception("Unexpected request");
            }
        }, () => closed++);
        await editor.ReloadAsync(); Assert.True(await editor.ReviewAsync()); using var cancellation = new CancellationTokenSource();
        var pending = editor.OutputStatusAsync(saved.GetProperty("outputs")[0].GetProperty("id").GetGuid(), 0, 1, cancellation.Token);
        await entered.Task.WaitAsync(TimeSpan.FromSeconds(5)); cancellation.Cancel(); await Assert.ThrowsAnyAsync<OperationCanceledException>(() => pending);
        Assert.Equal(HubEditorState.Uncertain, editor.State); Assert.Null(editor.LastOutputObservation); editor.Dispose(); Assert.Equal(1, closed);
        Assert.Equal("ready", (await host.Command(new { op = "hostStatus" })).GetProperty("phase").GetString());
        for (int i = 0; i < 3; i++) Assert.Equal(0, (await host.Status(i)).GetProperty("leaseCount").GetInt32());
    }
    [Fact]
    public async Task NativeOutputDiagnosticWindowShowsUnsafeSimulationAndHostPagination()
    {
        await Wpf(async () => {
            await using var host = await Host.Open(); var window = new HubConfigurationWindow(host.Executable, host.ConfigPath, host.Selection(0, "switch").InstanceId);
            try {
                window.Show(); await UiUntil(() => Controls<Button>(window).Single(b => (string)b.Content == "Review changes").IsEnabled);
                Controls<TabControl>(window).Single().SelectedIndex = 5;
                var selected = Controls<ComboBox>(window).Single(c => (string?)c.Tag == "diagnostic-output"); selected.SelectedIndex = 1;
                var read = Controls<Button>(window).Single(b => (string)b.Content == "Read cached output health");
                read.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                await UiUntil(() => read.IsEnabled && Controls<TextBlock>(window).Single(t => (string?)t.Tag == "output-summary").Text.Contains("UNSAFE"));
                Assert.Contains("raw unknown", Controls<TextBlock>(window).Single(t => (string?)t.Tag == "output-summary").Text);
                await Capture(window, "hub-native-output-diagnostics.png");
                selected.SelectedIndex = 0; var limit = Controls<TextBox>(window).Single(t => (string?)t.Tag == "diagnostic-limit"); limit.Text = "1";
                read.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); var next = Controls<Button>(window).Single(b => (string)b.Content == "Read next output items"); await UiUntil(() => next.IsEnabled);
                next.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(() => read.IsEnabled);
                Assert.Equal("1", Controls<TextBox>(window).Single(t => (string?)t.Tag == "diagnostic-start").Text);
                for (int i = 0; i < 3; i++) Assert.Equal(0, (await host.Status(i)).GetProperty("leaseCount").GetInt32());
            } finally { window.Close(); }
        });
    }
}
