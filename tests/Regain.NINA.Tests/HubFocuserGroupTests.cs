using System.Text.Json;
using System.Text.Json.Nodes;
using System.Windows;
using System.Windows.Controls;
using Regain.Hub;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class HubFocuserGroupContractTests
{
    internal static JsonObject Configuration()
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);
        while (directory is not null && !File.Exists(Path.Combine(directory.FullName, "Cargo.toml"))) directory = directory.Parent;
        return JsonNode.Parse(File.ReadAllText(Path.Combine(directory!.FullName, "crates", "regain-hub", "examples", "paired-focusers.json")))!.AsObject();
    }
    private static JsonElement Description() => JsonNode.Parse(File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "hub-config.json")))!
        ["coordination"]!["focuserGroups"]!.Deserialize<JsonElement>();
    private static JsonElement Element(JsonNode node) => JsonSerializer.SerializeToElement(node);
    private static readonly Guid Host = Guid.NewGuid();
    private static JsonObject Complete(JsonObject config, Guid operation)
    {
        var group = config["focuserGroups"]![0]!;
        var members = new JsonArray(); var bindings = new JsonArray();
        for (var i = 0; i < 2; i++) {
            var source = config["sources"]![i]!["id"]!.GetValue<string>(); var configured = group["members"]![i]!["source"]!.GetValue<string>();
            var target = 50000 + group["members"]![i]!["offset"]!.GetValue<int>();
            bindings.Add(new JsonObject { ["configuredSource"] = configured, ["physicalSource"] = source });
            members.Add(new JsonObject { ["source"] = source, ["generation"] = Guid.NewGuid().ToString(), ["target"] = target, ["phase"] = "complete", ["lastPosition"] = target, ["error"] = null });
        }
        return new JsonObject { ["hostInstance"] = Host.ToString(), ["configurationRevision"] = config["revision"]!.DeepClone(),
            ["operation"] = operation.ToString(), ["group"] = group["id"]!.DeepClone(), ["logicalTarget"] = 50000, ["sequence"] = 3, ["phase"] = "complete", ["bindings"] = bindings, ["failedSource"] = null, ["error"] = null,
            ["result"] = new JsonObject { ["operation"] = operation.ToString(), ["group"] = group["id"]!.DeepClone(), ["logicalTarget"] = 50000, ["sequence"] = 2, ["phase"] = "complete", ["members"] = members } };
    }
    [Theory]
    [InlineData("host")][InlineData("revision")][InlineData("operation")][InlineData("target")]
    [InlineData("alias")][InlineData("physical")][InlineData("duplicates")][InlineData("phase")]
    [InlineData("memberTarget")][InlineData("position")][InlineData("generation")][InlineData("unknown")]
    public async Task GroupRepliesRejectCorruptionBeforePublishingSuccess(string fault)
    {
        var config = Configuration(); var id = config["focuserGroups"]![0]!["id"]!.GetValue<string>(); var operation = Guid.NewGuid();
        var result = Complete(config, operation);
        switch (fault) {
            case "host": result["hostInstance"] = Guid.NewGuid().ToString(); break;
            case "revision": result["configurationRevision"] = Guid.NewGuid().ToString(); break;
            case "operation": result["result"]!["operation"] = Guid.NewGuid().ToString(); break;
            case "target": result["logicalTarget"] = 50001; break;
            case "alias": result["bindings"]![0]!["configuredSource"] = Guid.NewGuid().ToString(); break;
            case "physical": result["bindings"]![0]!["physicalSource"] = Guid.NewGuid().ToString(); break;
            case "duplicates": result["bindings"]![1]!["physicalSource"] = result["bindings"]![0]!["physicalSource"]!.DeepClone(); break;
            case "phase": result["result"]!["phase"] = "moving"; break;
            case "memberTarget": result["result"]!["members"]![1]!["target"] = 50001; break;
            case "position": result["result"]!["members"]![1]!["lastPosition"] = 50001; break;
            case "generation": result["result"]!["members"]![1]!["generation"] = Guid.Empty.ToString(); break;
            case "unknown": result["unexpected"] = true; break;
        }
        using var groups = new HubFocuserGroups(Host, Description(), Element(config), (_, _) => Task.FromResult(Element(result)), () => { });
        var error = await Assert.ThrowsAsync<HubException>(() => groups.StatusAsync(Guid.Parse(id), operation));
        Assert.Equal(HubFailure.Protocol, error.Failure);
    }
    [Fact]
    public async Task UnknownStartIsNeverReplayedAndStatusMustKeepSequenceAndTarget()
    {
        var config = Configuration(); var group = config["focuserGroups"]![0]!["id"]!.GetValue<Guid>();
        var complete = Complete(config, Guid.NewGuid()); var starts = 0;
        using var groups = new HubFocuserGroups(Host, Description(), Element(config), (command, _) => {
            Assert.Equal(config["revision"]!.GetValue<Guid>(), command.GetProperty("expectedRevision").GetGuid());
            if (command.GetProperty("op").GetString() == "startFocuserGroup") { starts++; throw new HubException(HubFailure.Uncertain); }
            return Task.FromResult(Element(complete));
        }, () => { });
        await Assert.ThrowsAsync<HubException>(() => groups.StartAsync(group, 50000));
        var observed = await groups.StatusAsync(group);
        Assert.True(HubFocuserGroups.Terminal(observed)); Assert.Contains("50200", HubFocuserGroups.Summary(observed));
        await Assert.ThrowsAsync<InvalidOperationException>(() => groups.StartAsync(group, 50000)); Assert.Equal(1, starts);
        complete["sequence"] = 2;
        await Assert.ThrowsAsync<HubException>(() => groups.StatusAsync(group));
        complete["sequence"] = 4; complete["logicalTarget"] = 50001;
        await Assert.ThrowsAsync<HubException>(() => groups.StatusAsync(group));
        Assert.Throws<InvalidOperationException>(() => groups.ParseTarget(group, "NaN"));
        Assert.Throws<InvalidOperationException>(() => groups.ParseTarget(group, "51001"));
        Assert.Equal(49000, groups.ParseTarget(group, "49000"));
    }
    [Fact]
    public void GeneratedGroupDraftKeepsIdentityAndCalibrationWithCapabilityGating()
    {
        var node = JsonNode.Parse(File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "hub-config.json")))!;
        var saved = Configuration();
        var gated = new HubConfigurationDraft(Element(node), Element(saved)); Assert.False(gated.Field("/focuserGroups").Enabled);
        node["capabilities"] = new JsonArray("focuserGroups");
        var draft = new HubConfigurationDraft(Element(node), Element(saved));
        Assert.Throws<InvalidOperationException>(() => draft.SetValue("/focuserGroups/0/id", JsonSerializer.SerializeToElement(Guid.NewGuid())));
        draft.SetValue("/focuserGroups/0/label", JsonSerializer.SerializeToElement("Renamed pair"));
        draft.SetValue("/focuserGroups/0/members/1/offset", JsonSerializer.SerializeToElement(-200));
        Assert.Contains("Renamed pair", draft.Preview()); Assert.Contains("-200", draft.Preview());
        draft.AddItem("/focuserGroups");
        Assert.NotEqual(Guid.Empty, draft.Field("/focuserGroups/1/id").Value!.Value.GetGuid());
        Assert.Equal(120, draft.Field("/focuserGroups/1/timeoutSeconds").Value!.Value.GetDouble());
        draft.AddItem("/focuserGroups/1/members");
        Assert.Equal(1, draft.Field("/focuserGroups/1/members/0/scaleNumerator").Value!.Value.GetInt32());
        Assert.Equal(1, draft.Field("/focuserGroups/1/members/0/scaleDenominator").Value!.Value.GetInt32());
    }
}

public sealed partial class HubNativeTests
{
    private sealed class GroupProgress(Action<string> report) : IProgress<global::NINA.Core.Model.ApplicationStatus>
    {
        public void Report(global::NINA.Core.Model.ApplicationStatus value) => report(value.Status);
    }
    private static MoveHubFocuserGroup GroupStep(Host host, Action? attached = null)
    {
        var step = new MoveHubFocuserGroup((path, instance, token) => { attached?.Invoke(); return HubFocuserGroups.AttachAsync(host.Executable, path, instance, token); });
        step.SelectGroup(host.ConfigPath, host.Client.Hello.InstanceId, host.Config["focuserGroups"]![0]!["id"]!.GetValue<Guid>(), "Paired focusers [SIMULATION]");
        step.Target = 50100; return step;
    }
    private static void PairedFocusers(JsonObject config)
    {
        var pair = HubFocuserGroupContractTests.Configuration();
        config["sources"] = pair["sources"]!.DeepClone(); config["outputs"] = pair["outputs"]!.DeepClone(); config["focuserGroups"] = pair["focuserGroups"]!.DeepClone();
    }
    [Fact]
    public async Task FocuserGroupSequenceUsesNativeIpcAndPersistsFailedOperationFenceAcrossCloneAndSave()
    {
        await using var host = await Host.Open(PairedFocusers);
        var attachments = 0; var step = GroupStep(host, () => attachments++);
        var statuses = new List<string>();
        await step.Execute(new GroupProgress(statuses.Add), CancellationToken.None);
        Assert.Contains("Group complete", step.Outcome); Assert.False(step.ReconciliationRequired);
        Assert.Contains("50300", step.Outcome); Assert.NotEmpty(statuses);
        var clone = Assert.IsType<MoveHubFocuserGroup>(step.Clone()); Assert.Equal(step.GroupId, clone.GroupId); Assert.Equal(step.Target, clone.Target);
        await host.Update(1, new { focuser = new { tempComp = true } });
        await Assert.ThrowsAsync<IOException>(() => step.Execute(new GroupProgress(_ => { }), CancellationToken.None));
        Assert.True(step.ReconciliationRequired); Assert.Contains("preflightFailed", step.Outcome);
        var before = attachments;
        await Assert.ThrowsAsync<InvalidOperationException>(() => step.Execute(new GroupProgress(_ => { }), CancellationToken.None)); Assert.Equal(before, attachments);
        clone = Assert.IsType<MoveHubFocuserGroup>(step.Clone()); Assert.True(clone.ReconciliationRequired); Assert.False(clone.Validate());
        var saved = Newtonsoft.Json.JsonConvert.SerializeObject(step);
        Assert.DoesNotContain("Outcome", saved); Assert.DoesNotContain("members", saved);
        var restored = Newtonsoft.Json.JsonConvert.DeserializeObject<MoveHubFocuserGroup>(saved)!;
        Assert.True(restored.ReconciliationRequired); Assert.Equal(step.GroupId, restored.GroupId);
        step.SelectGroup(step.ConfigPath, step.InstanceId, step.GroupId, step.GroupLabel); Assert.True(step.ReconciliationRequired);
        step.AllowNewOperationAfterInspection(); await host.Update(1, new { focuser = new { tempComp = false } });
        await step.Execute(new GroupProgress(_ => { }), CancellationToken.None); Assert.False(step.ReconciliationRequired);
        await Wpf(() => {
            var resources = new HubSequenceTemplates();
            // NINA's SequenceBlockView drag/drop requires its real Application;
            // this private test checks the exported dictionary and typed key.
            Assert.IsType<DataTemplate>(resources[new DataTemplateKey(typeof(MoveHubFocuserGroup))]); return Task.CompletedTask;
        });
    }
    [Fact]
    public async Task FocuserGroupSequenceCancellationRequestsOnlyItsKnownOperationAndDoesNotPermitAutomaticRetry()
    {
        await using var host = await Host.Open(PairedFocusers);
        await host.Update(0, new { focuser = new { moveDurationSeconds = 2.0 } });
        var step = GroupStep(host); using var cancel = new CancellationTokenSource();
        var running = step.Execute(new GroupProgress(_ => { }), cancel.Token);
        await Eventually(() => Task.FromResult(step.Outcome.Contains("Group running")));
        Assert.Throws<InvalidOperationException>(() => step.AllowNewOperationAfterInspection());
        await Assert.ThrowsAsync<InvalidOperationException>(() => step.Execute(new GroupProgress(_ => { }), CancellationToken.None));
        cancel.Cancel(); await Assert.ThrowsAnyAsync<OperationCanceledException>(() => running);
        Assert.True(step.ReconciliationRequired); Assert.Contains("no Halt was sent", step.Outcome);
        using var groups = await HubFocuserGroups.AttachAsync(host.Executable, host.ConfigPath, host.Client.Hello.InstanceId);
        JsonElement result = default;
        await Eventually(async () => { result = await groups.StatusAsync(step.GroupId); return HubFocuserGroups.Terminal(result); });
        Assert.Equal("cancelled", result.GetProperty("phase").GetString());
        await Assert.ThrowsAsync<InvalidOperationException>(() => step.Execute(new GroupProgress(_ => { }), CancellationToken.None));
    }
    [Fact]
    public async Task NativeGroupReattachesWithoutAnOutputConnectionAndCompletesBothCalibratedTargets()
    {
        await using var host = await Host.Open(PairedFocusers);
        using var groups = await HubFocuserGroups.AttachAsync(host.Executable, host.ConfigPath, host.Client.Hello.InstanceId);
        var group = Assert.Single(groups.Groups).GetProperty("id").GetGuid();
        var absent = await Assert.ThrowsAsync<HubException>(() => groups.StatusAsync(group)); Assert.Equal("unavailable", absent.Remote?.Code);
        for (var i = 0; i < 3; i++) Assert.Equal(0, (await host.Status(i)).GetProperty("leaseCount").GetInt32());
        await host.Update(0, new { focuser = new { moveDurationSeconds = 0.5 } });
        var started = await groups.StartAsync(group, 50100); var operation = started.GetProperty("operation").GetGuid();
        groups.Dispose();
        using var reattached = await HubFocuserGroups.AttachAsync(host.Executable, host.ConfigPath, host.Client.Hello.InstanceId);
        JsonElement result = default;
        await Eventually(async () => { result = await reattached.StatusAsync(group, operation); return HubFocuserGroups.Terminal(result); });
        Assert.Equal("complete", result.GetProperty("phase").GetString());
        Assert.Equal(new[] { 50100, 50300 }, result.GetProperty("result").GetProperty("members").EnumerateArray().Select(m => m.GetProperty("lastPosition").GetInt32()));
        Assert.Equal(reattached.HostInstance, result.GetProperty("hostInstance").GetGuid());
        await Eventually(async () => (await host.Status(0)).GetProperty("leaseCount").GetInt32() == 0 && (await host.Status(1)).GetProperty("leaseCount").GetInt32() == 0);
        Assert.Equal(0, (await host.Status(2)).GetProperty("leaseCount").GetInt32());
    }
    [Fact]
    public async Task NativeGroupControlsRequireStatusAndRetainCompletionAfterTheWindowCloses()
    {
        await using var host = await Host.Open(PairedFocusers);
        await Wpf(async () => {
            var window = new HubConfigurationWindow(host.Executable, host.ConfigPath, host.Client.Hello.InstanceId);
            try {
                window.Show(); await UiUntil(() => Controls<Button>(window).Any(b => (string)b.Content == "Read retained group status"));
                var tab = Controls<TabItem>(window).Single(t => (string)t.Header == "Focuser groups");
                Controls<TabControl>(window).Single().SelectedItem = tab;
                var start = Controls<Button>(window).Single(b => (string)b.Content == "Start calibrated group move"); Assert.False(start.IsEnabled);
                var read = Controls<Button>(window).Single(b => (string)b.Content == "Read retained group status");
                await UiUntil(() => read.IsEnabled); read.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(() => start.IsEnabled);
                var target = Controls<TextBox>(window).Single(t => (string?)t.Tag == "group-target"); target.Text = "NaN"; Assert.False(start.IsEnabled);
                target.Text = "50050"; Assert.True(start.IsEnabled);
                start.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(() => read.IsEnabled && !start.IsEnabled);
                Assert.Contains("operation", Controls<TextBlock>(window).Single(t => (string?)t.Tag == "group-summary").Text);
                await Capture(window, "hub-focuser-groups-simulation.png"); window.Close();
            } finally { window.Close(); }
        });
        using var groups = await HubFocuserGroups.AttachAsync(host.Executable, host.ConfigPath, host.Client.Hello.InstanceId);
        var group = groups.Groups.Single().GetProperty("id").GetGuid(); JsonElement result = default;
        await Eventually(async () => { result = await groups.StatusAsync(group); return HubFocuserGroups.Terminal(result); });
        Assert.Equal("complete", result.GetProperty("phase").GetString());
        await Wpf(async () => {
            var reopened = new HubConfigurationWindow(host.Executable, host.ConfigPath, host.Client.Hello.InstanceId);
            try {
                reopened.Show(); await UiUntil(() => Controls<Button>(reopened).Any(b => (string)b.Content == "Read retained group status"));
                Controls<TabControl>(reopened).Single().SelectedItem = Controls<TabItem>(reopened).Single(t => (string)t.Header == "Focuser groups");
                var read = Controls<Button>(reopened).Single(b => (string)b.Content == "Read retained group status");
                await UiUntil(() => read.IsEnabled); read.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                await UiUntil(() => Controls<TextBlock>(reopened).Single(t => (string?)t.Tag == "group-summary").Text.Contains("Group complete"));
                var summary = Controls<TextBlock>(reopened).Single(t => (string?)t.Tag == "group-summary").Text;
                Assert.Contains(result.GetProperty("operation").GetString()!, summary); Assert.Contains("50250", summary);
                await Capture(reopened, "hub-focuser-group-results-simulation.png");
            } finally { reopened.Close(); }
        });
    }
}
