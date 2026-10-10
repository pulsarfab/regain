using System.Text.Json;
using System.Text.Json.Nodes;
using System.Windows;
using System.Windows.Controls;
using Regain.Hub;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class HubCameraGroupReplyTests
{
    private static readonly Guid Host = Guid.NewGuid();
    private static JsonElement Element(JsonNode node) => JsonSerializer.SerializeToElement(node);
    private static JsonElement Description() => JsonNode.Parse(File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "hub-config.json")))!
        ["coordination"]!["cameraGroups"]!.Deserialize<JsonElement>();
    private static Guid Group(JsonObject config) => config["cameraGroups"]![0]!["id"]!.GetValue<Guid>();
    private static JsonObject Complete(JsonObject config)
    {
        var group = config["cameraGroups"]![0]!; var operation = Guid.NewGuid().ToString();
        var members = new JsonArray(); var bindings = new JsonArray(); var requests = new JsonArray();
        for (var i = 0; i < 2; i++) {
            var source = config["sources"]![i]!["id"]!.GetValue<string>(); var configured = group["members"]![i]!.GetValue<string>();
            var generation = Guid.NewGuid().ToString(); var acquisition = Guid.NewGuid().ToString();
            var exposure = new JsonObject { ["durationSeconds"] = i + 1, ["light"] = true };
            bindings.Add(new JsonObject { ["configuredSource"] = configured, ["physicalSource"] = source });
            requests.Add(new JsonObject { ["source"] = configured, ["exposure"] = exposure.DeepClone() });
            var image = new JsonObject { ["source"] = source, ["generation"] = generation, ["acquisition"] = acquisition, ["request"] = exposure.DeepClone(),
                ["geometry"] = new JsonObject { ["width"] = 64, ["height"] = 48, ["binX"] = 1, ["binY"] = 1, ["startX"] = 0, ["startY"] = 0 },
                ["exposure"] = new JsonObject { ["durationSeconds"] = i + 1, ["startTime"] = "2026-10-07T12:00:00", ["durationError"] = null, ["startTimeError"] = null } };
            members.Add(new JsonObject { ["source"] = source, ["generation"] = generation, ["acquisition"] = acquisition, ["request"] = exposure,
                ["phase"] = "complete", ["requireScalarImage"] = false, ["captureProfile"] = null, ["dispatchSeconds"] = 0.1 + i * 0.02, ["acknowledgementSeconds"] = 0.101 + i * 0.02, ["image"] = image, ["error"] = null, ["abortError"] = null });
        }
        return new JsonObject { ["hostInstance"] = Host.ToString(), ["configurationRevision"] = config["revision"]!.DeepClone(), ["operation"] = operation,
            ["group"] = group["id"]!.DeepClone(), ["sequence"] = 3, ["phase"] = "complete", ["requests"] = requests, ["bindings"] = bindings, ["failedSource"] = null, ["error"] = null,
            ["result"] = new JsonObject { ["operation"] = operation, ["group"] = group["id"]!.DeepClone(), ["sequence"] = 2, ["phase"] = "complete", ["startSkewSeconds"] = 0.02, ["members"] = members } };
    }
    [Theory]
    [InlineData("host")][InlineData("revision")][InlineData("operation")][InlineData("group")][InlineData("sequence")]
    [InlineData("alias")][InlineData("physical")][InlineData("duplicate")][InlineData("duration")][InlineData("request")]
    [InlineData("imageSource")][InlineData("imageGeneration")][InlineData("imageAcquisition")][InlineData("imageRequest")]
    [InlineData("imageMissing")][InlineData("imageUnexpected")][InlineData("geometry")][InlineData("generation")]
    [InlineData("dispatch")][InlineData("ack")][InlineData("skew")][InlineData("phase")][InlineData("failedSource")][InlineData("unknown")]
    public async Task CorruptedCameraResultsCannotPublishSuccess(string fault)
    {
        var config = HubCameraGroupContractTests.Configuration(); var complete = Complete(config); var operation = Guid.Parse(complete["operation"]!.GetValue<string>());
        var member = complete["result"]!["members"]![0]!;
        switch (fault) {
            case "host": complete["hostInstance"] = Guid.NewGuid().ToString(); break;
            case "revision": complete["configurationRevision"] = Guid.NewGuid().ToString(); break;
            case "operation": complete["result"]!["operation"] = Guid.NewGuid().ToString(); break;
            case "group": complete["result"]!["group"] = Guid.NewGuid().ToString(); break;
            case "sequence": complete["sequence"] = 0; break;
            case "alias": complete["bindings"]![0]!["configuredSource"] = Guid.NewGuid().ToString(); break;
            case "physical": complete["bindings"]![0]!["physicalSource"] = Guid.NewGuid().ToString(); break;
            case "duplicate": complete["bindings"]![1]!["physicalSource"] = complete["bindings"]![0]!["physicalSource"]!.DeepClone(); break;
            case "duration": complete["requests"]![0]!["exposure"]!["durationSeconds"] = -1; break;
            case "request": member["request"]!["light"] = false; break;
            case "imageSource": member["image"]!["source"] = Guid.NewGuid().ToString(); break;
            case "imageGeneration": member["image"]!["generation"] = Guid.NewGuid().ToString(); break;
            case "imageAcquisition": member["image"]!["acquisition"] = Guid.NewGuid().ToString(); break;
            case "imageRequest": member["image"]!["request"]!["light"] = false; break;
            case "imageMissing": member["image"] = null; break;
            case "imageUnexpected": member["phase"] = "exposing"; break;
            case "geometry": member["image"]!["geometry"]!["width"] = 0; break;
            case "generation": member["generation"] = Guid.Empty.ToString(); break;
            case "dispatch": member["dispatchSeconds"] = -1; break;
            case "ack": member["acknowledgementSeconds"] = 0; break;
            case "skew": complete["result"]!["startSkewSeconds"] = 0.5; break;
            case "phase": complete["result"]!["phase"] = "capturing"; break;
            case "failedSource": complete["failedSource"] = Guid.NewGuid().ToString(); break;
            case "unknown": complete["unexpected"] = true; break;
        }
        using var groups = new HubCameraGroups(Host, Description(), Element(config), (_, _) => Task.FromResult(Element(complete)), () => { });
        var error = await Assert.ThrowsAsync<HubException>(() => groups.StatusAsync(Group(config), operation)); Assert.Equal(HubFailure.Protocol, error.Failure);
    }
    [Fact]
    public async Task UnknownCaptureNeverReplaysEvenAfterStatusAndMalformedRequestsPerformNoIo()
    {
        var config = HubCameraGroupContractTests.Configuration(); var complete = Complete(config); var starts = 0; var calls = 0;
        using var groups = new HubCameraGroups(Host, Description(), Element(config), (command, _) => {
            calls++; Assert.Equal(config["revision"]!.GetValue<Guid>(), command.GetProperty("expectedRevision").GetGuid());
            if (command.GetProperty("op").GetString() == "startCameraGroup") { starts++; throw new HubException(HubFailure.Uncertain); }
            return Task.FromResult(Element(complete));
        }, () => { });
        var group = Group(config); var exposures = groups.UniformRequests(group, 1, true);
        Assert.Throws<ArgumentOutOfRangeException>(() => groups.UniformRequests(group, double.NaN, true));
        await Assert.ThrowsAsync<ArgumentException>(() => groups.StartAsync(group, exposures.Reverse().ToArray())); Assert.Equal(0, calls);
        await Assert.ThrowsAsync<HubException>(() => groups.StartAsync(group, exposures));
        var status = await groups.StatusAsync(group); Assert.True(HubCameraGroups.Terminal(status)); Assert.Contains("sensor synchronization is not guaranteed", HubCameraGroups.Summary(status));
        await Assert.ThrowsAsync<InvalidOperationException>(() => groups.StartAsync(group, exposures)); Assert.Equal(1, starts);
        complete["sequence"] = 4; complete["result"]!["startSkewSeconds"] = 0.021;
        await Assert.ThrowsAsync<HubException>(() => groups.StatusAsync(group));
    }
    [Theory, InlineData("sequence"), InlineData("innerSequence"), InlineData("generation"), InlineData("acquisition"), InlineData("image"), InlineData("requests"), InlineData("disappear")]
    public async Task LiveStatusCannotRegressOrRetargetRetainedImages(string change)
    {
        var config = HubCameraGroupContractTests.Configuration(); var result = Complete(config);
        result["phase"] = "running"; result["result"]!["phase"] = "capturing";
        using var groups = new HubCameraGroups(Host, Description(), Element(config), (_, _) => Task.FromResult(Element(result)), () => { });
        await groups.StatusAsync(Group(config)); result["sequence"] = 4; result["result"]!["sequence"] = 3;
        var member = result["result"]!["members"]![0]!;
        switch (change) {
            case "sequence": result["sequence"] = 2; break;
            case "innerSequence": result["result"]!["sequence"] = 1; break;
            case "generation": member["generation"] = Guid.NewGuid().ToString(); member["image"]!["generation"] = member["generation"]!.DeepClone(); break;
            case "acquisition": member["acquisition"] = Guid.NewGuid().ToString(); member["image"]!["acquisition"] = member["acquisition"]!.DeepClone(); break;
            case "image": member["image"]!["geometry"]!["width"] = 65; break;
            case "requests": result["requests"]![0]!["exposure"]!["durationSeconds"] = 3; break;
            case "disappear": result["result"] = null; break;
        }
        var error = await Assert.ThrowsAsync<HubException>(() => groups.StatusAsync(Group(config))); Assert.Equal(HubFailure.Protocol, error.Failure);
    }
    [Fact]
    public async Task LateAbortFailureMayBeReportedWithoutChangingAnAlreadyCompletedImage()
    {
        var config = HubCameraGroupContractTests.Configuration(); var result = Complete(config);
        result["phase"] = "running"; result["result"]!["phase"] = "capturing";
        using var groups = new HubCameraGroups(Host, Description(), Element(config), (_, _) => Task.FromResult(Element(result)), () => { });
        await groups.StatusAsync(Group(config)); result["sequence"] = 4; result["result"]!["sequence"] = 3;
        result["result"]!["members"]![0]!["abortError"] = new JsonObject { ["kind"] = "busy", ["message"] = "Capture completed while abort was pending" };
        Assert.Contains("abort:", HubCameraGroups.Summary(await groups.StatusAsync(Group(config))));
    }
    [Theory, InlineData(false), InlineData(true)]
    public async Task TerminalSequencesCompareExactlyBeyondDoubleIntegerPrecision(bool inner)
    {
        var config = HubCameraGroupContractTests.Configuration(); var result = Complete(config);
        var target = inner ? result["result"]! : result; target["sequence"] = 9007199254740992UL;
        using var groups = new HubCameraGroups(Host, Description(), Element(config), (_, _) => Task.FromResult(Element(result)), () => { });
        await groups.StatusAsync(Group(config)); target["sequence"] = 9007199254740993UL;
        var error = await Assert.ThrowsAsync<HubException>(() => groups.StatusAsync(Group(config))); Assert.Equal(HubFailure.Protocol, error.Failure);
    }
    [Theory, InlineData("missing"), InlineData("maxAdu"), InlineData("sensorType"), InlineData("bayer"), InlineData("requirement"), InlineData("sensorName")]
    public async Task ScalarProfileMustMatchTheRequestedFormatAndRemainValid(string fault)
    {
        var config = HubCameraGroupContractTests.Configuration(); var result = Complete(config); var member = result["result"]!["members"]![0]!;
        result["requests"]![0]!["requireScalarImage"] = true; member["requireScalarImage"] = true;
        var profile = new JsonObject { ["maxAdu"] = 65535, ["sensorType"] = 0, ["bayerOffsetX"] = 0, ["bayerOffsetY"] = 0, ["sensorName"] = "Frozen sensor" };
        member["captureProfile"] = profile;
        switch (fault) {
            case "missing": member["captureProfile"] = null; break;
            case "maxAdu": profile["maxAdu"] = 0; break;
            case "sensorType": profile["sensorType"] = 1; break;
            case "bayer": profile["bayerOffsetX"] = 1; break;
            case "requirement": member["requireScalarImage"] = false; break;
            case "sensorName": profile.Remove("sensorName"); break;
        }
        using var groups = new HubCameraGroups(Host, Description(), Element(config), (_, _) => Task.FromResult(Element(result)), () => { });
        var error = await Assert.ThrowsAsync<HubException>(() => groups.StatusAsync(Group(config))); Assert.Equal(HubFailure.Protocol, error.Failure);
    }
    [Fact]
    public async Task PublishedScalarProfileCannotChangeWithALaterRunningReport()
    {
        var config = HubCameraGroupContractTests.Configuration(); var result = Complete(config);
        result["phase"] = "running"; result["result"]!["phase"] = "capturing";
        result["requests"]![0]!["requireScalarImage"] = true;
        var member = result["result"]!["members"]![0]!; member["requireScalarImage"] = true;
        member["captureProfile"] = new JsonObject { ["maxAdu"] = 65535, ["sensorType"] = 0, ["bayerOffsetX"] = 0, ["bayerOffsetY"] = 0, ["sensorName"] = "Frozen sensor" };
        using var groups = new HubCameraGroups(Host, Description(), Element(config), (_, _) => Task.FromResult(Element(result)), () => { });
        await groups.StatusAsync(Group(config)); result["sequence"] = 4; result["result"]!["sequence"] = 3;
        member["captureProfile"]!["sensorName"] = "Different valid name";
        var error = await Assert.ThrowsAsync<HubException>(() => groups.StatusAsync(Group(config))); Assert.Equal(HubFailure.Protocol, error.Failure);
    }
    [Theory, InlineData(false), InlineData(true)]
    public async Task GroupDownloadRejectsRetargetedOrWrongGeometryImagesAndReleasesTheirBudget(bool identityFault)
    {
        var config = HubCameraGroupContractTests.Configuration(); var result = Complete(config); var calls = 0; var budget = new HubImageBudget(65536);
        using var groups = new HubCameraGroups(Host, Description(), Element(config), (_, _) => Task.FromResult(Element(result)), () => { },
            (identity, suppliedBudget, _, _) => {
                calls++; Assert.Same(budget, suppliedBudget);
                var descriptor = new HubImageDescriptor(JsonSerializer.SerializeToElement(new { width = identityFault ? 64 : 65, height = 48,
                    planes = (int?)null, elementType = "byte", transmissionType = "byte", order = "ascom" }));
                var returned = identityFault ? new HubGroupImageRequest(identity.HostInstance, identity.ConfigurationRevision, identity.Group, Guid.NewGuid(), identity.Source, identity.Generation, identity.Acquisition) : identity;
                return Task.FromResult(new HubCameraImage(returned, descriptor, new byte[descriptor.ByteLength], budget.Reserve(descriptor.ByteLength)));
            });
        var group = Group(config); var source = groups.UniformRequests(group, 1, true)[0].Source; var operation = Guid.Parse(result["operation"]!.GetValue<string>());
        await Assert.ThrowsAsync<InvalidOperationException>(() => groups.DownloadAsync(group, operation, source, budget, TimeSpan.FromSeconds(1))); Assert.Equal(0, calls);
        await groups.StatusAsync(group, operation);
        var error = await Assert.ThrowsAsync<HubException>(() => groups.DownloadAsync(group, operation, source, budget, TimeSpan.FromSeconds(1)));
        Assert.Equal(HubFailure.Protocol, error.Failure); Assert.Equal(0, budget.UsedBytes); Assert.Equal(1, calls);
    }
}

public sealed partial class HubNativeTests
{
    private static void PairedCameras(JsonObject config)
    {
        var pair = HubCameraGroupContractTests.Configuration();
        config["sources"] = pair["sources"]!.DeepClone(); config["outputs"] = pair["outputs"]!.DeepClone(); config["cameraGroups"] = pair["cameraGroups"]!.DeepClone();
    }
    [Fact]
    public async Task CameraGroupReattachesAndDownloadsExactSeparateImagesWithoutOutputLeases()
    {
        await using var host = await Host.Open(PairedCameras);
        using var groups = await HubCameraGroups.AttachAsync(host.Executable, host.ConfigPath, host.Client.Hello.InstanceId);
        var group = Assert.Single(groups.Groups).GetProperty("id").GetGuid();
        var absent = await Assert.ThrowsAsync<HubException>(() => groups.StatusAsync(group)); Assert.Equal("unavailable", absent.Remote?.Code);
        for (var i = 0; i < 3; i++) Assert.Equal(0, (await host.Status(i)).GetProperty("leaseCount").GetInt32());
        var requests = groups.UniformRequests(group, 0.2, true).Select((r, i) => new HubCameraMemberRequest(r.Source, 0.2 + i * 0.2, r.Light)).ToArray();
        var started = await groups.StartAsync(group, requests); var operation = started.GetProperty("operation").GetGuid(); groups.Dispose();
        using var reattached = await HubCameraGroups.AttachAsync(host.Executable, host.ConfigPath, host.Client.Hello.InstanceId);
        JsonElement result = default;
        await Eventually(async () => { result = await reattached.StatusAsync(group, operation); return HubCameraGroups.Terminal(result); });
        Assert.Equal("complete", result.GetProperty("phase").GetString());
        Assert.Equal(new[] { 0.2, 0.4 }, result.GetProperty("result").GetProperty("members").EnumerateArray().Select(m => m.GetProperty("request").GetProperty("durationSeconds").GetDouble()));
        await Eventually(async () => (await host.Status(0)).GetProperty("leaseCount").GetInt32() == 0 && (await host.Status(1)).GetProperty("leaseCount").GetInt32() == 0);
        var budget = new HubImageBudget(320 * 240 * 2 * 2);
        using var first = await reattached.DownloadAsync(group, operation, requests[0].Source, budget, TimeSpan.FromSeconds(10));
        using var second = await reattached.DownloadAsync(group, operation, requests[1].Source, budget, TimeSpan.FromSeconds(10));
        Assert.IsType<HubGroupImageRequest>(first.Request); Assert.NotEqual(first.Request.Acquisition, second.Request.Acquisition); Assert.NotEqual(first.Request.Source, second.Request.Source);
        Assert.Equal(320, first.Descriptor.Width); Assert.Equal(240, second.Descriptor.Height); Assert.Equal(first.ByteLength + second.ByteLength, budget.UsedBytes);
        var bytes = new byte[first.ByteLength > 65536 ? 65536 : first.ByteLength]; first.CopyTo(0, bytes, 0, bytes.Length);
        using var again = await reattached.DownloadAsync(group, operation, requests[0].Source, new HubImageBudget(first.ByteLength), TimeSpan.FromSeconds(10));
        var repeated = new byte[bytes.Length]; again.CopyTo(0, repeated, 0, repeated.Length); Assert.Equal(bytes, repeated); Assert.Equal(first.Request.Acquisition, again.Request.Acquisition);
        first.Dispose(); second.Dispose(); Assert.Equal(0, budget.UsedBytes);
        for (var i = 0; i < 3; i++) Assert.Equal(0, (await host.Status(i)).GetProperty("leaseCount").GetInt32());
        await reattached.StartAsync(group, requests);
        await Assert.ThrowsAsync<InvalidOperationException>(() => reattached.DownloadAsync(group, operation, requests[0].Source, budget, TimeSpan.FromSeconds(10)));
    }
    [Theory, InlineData("abortStarted"), InlineData("leaveRunning")]
    public async Task CameraCancellationUsesSavedPolicyAndNeverFabricatesImages(string policy)
    {
        await using var host = await Host.Open(config => { PairedCameras(config); config["cameraGroups"]![0]!["cancellationPolicy"] = policy; });
        using var groups = await HubCameraGroups.AttachAsync(host.Executable, host.ConfigPath, host.Client.Hello.InstanceId);
        var group = groups.Groups.Single().GetProperty("id").GetGuid();
        var started = await groups.StartAsync(group, groups.UniformRequests(group, 2, true)); var operation = started.GetProperty("operation").GetGuid();
        await Eventually(async () => { var status = await groups.StatusAsync(group, operation); return status.GetProperty("result").ValueKind != JsonValueKind.Null && status.GetProperty("result").GetProperty("members").EnumerateArray().All(m => m.GetProperty("phase").GetString() == "exposing"); });
        await groups.CancelAsync(group, operation); JsonElement terminal = default;
        await Eventually(async () => { terminal = await groups.StatusAsync(group, operation); return HubCameraGroups.Terminal(terminal); });
        Assert.Equal("cancelled", terminal.GetProperty("phase").GetString());
        Assert.All(terminal.GetProperty("result").GetProperty("members").EnumerateArray(), member => {
            Assert.Null(member.GetProperty("image").GetString()); Assert.Equal(policy == "abortStarted" ? "aborted" : "exposing", member.GetProperty("phase").GetString());
        });
        await Assert.ThrowsAsync<InvalidOperationException>(() => groups.DownloadAsync(group, operation, groups.UniformRequests(group, 1, true)[0].Source, HubImageBudget.Shared, TimeSpan.FromSeconds(10)));
    }
    [Fact]
    public async Task NativeCameraControlsRequireInspectionAndRetainCaptureAfterWindowCloses()
    {
        await using var host = await Host.Open(PairedCameras);
        await Wpf(async () => {
            var window = new HubConfigurationWindow(host.Executable, host.ConfigPath, host.Client.Hello.InstanceId);
            try {
                window.Show(); await UiUntil(() => Controls<Button>(window).Any(b => (string)b.Content == "Read retained camera status"));
                Controls<TabControl>(window).Single().SelectedItem = Controls<TabItem>(window).Single(t => (string)t.Header == "Camera groups");
                var start = Controls<Button>(window).Single(b => (string)b.Content == "Start camera group capture"); Assert.False(start.IsEnabled);
                var read = Controls<Button>(window).Single(b => (string)b.Content == "Read retained camera status");
                await UiUntil(() => read.IsEnabled); read.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(() => start.IsEnabled);
                var durations = Controls<TextBox>(window).Where(t => (string?)t.Tag == "camera-group-duration").ToArray(); Assert.Equal(2, durations.Length);
                durations[0].Text = "NaN"; Assert.False(start.IsEnabled); durations[0].Text = "0.2"; durations[1].Text = "0.4"; Assert.True(start.IsEnabled);
                Assert.Contains("abort acknowledged captures", Controls<TextBlock>(window).Single(t => (string?)t.Tag == "camera-group-policy").Text);
                start.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(() => read.IsEnabled && !start.IsEnabled);
                Assert.Contains("operation", Controls<TextBlock>(window).Single(t => (string?)t.Tag == "camera-group-summary").Text);
                window.Close();
            } finally { window.Close(); }
        });
        using var groups = await HubCameraGroups.AttachAsync(host.Executable, host.ConfigPath, host.Client.Hello.InstanceId);
        var group = groups.Groups.Single().GetProperty("id").GetGuid(); JsonElement result = default;
        await Eventually(async () => { result = await groups.StatusAsync(group); return HubCameraGroups.Terminal(result); }); Assert.Equal("complete", result.GetProperty("phase").GetString());
        await Wpf(async () => {
            var reopened = new HubConfigurationWindow(host.Executable, host.ConfigPath, host.Client.Hello.InstanceId);
            try {
                reopened.Show(); await UiUntil(() => Controls<Button>(reopened).Any(b => (string)b.Content == "Read retained camera status"));
                Controls<TabControl>(reopened).Single().SelectedItem = Controls<TabItem>(reopened).Single(t => (string)t.Header == "Camera groups");
                var read = Controls<Button>(reopened).Single(b => (string)b.Content == "Read retained camera status"); await UiUntil(() => read.IsEnabled);
                read.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(() => Controls<Button>(reopened).Single(b => (string)b.Content == "Start camera group capture").IsEnabled);
                Assert.Contains("retained image", Controls<TextBlock>(reopened).Single(t => (string?)t.Tag == "camera-group-summary").Text);
                await Capture(reopened, "hub-camera-group-results-simulation.png");
            } finally { reopened.Close(); }
        });
    }
}
