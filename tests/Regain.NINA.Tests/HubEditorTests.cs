using System.Text.Json;
using System.Text.Json.Nodes;
using Regain.Hub;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    private static Task<HubEditorSession> Editor(Host host) => HubEditorSession.AttachAsync(host.Executable, host.ConfigPath, host.Selection(0, "switch").InstanceId);
    [Theory]
    [InlineData("focuser")]
    [InlineData("rotator")]
    public async Task NativeEditorCreatesSharedTypedOutputsFromHostDescriptorsWithoutOpeningEquipment(string type)
    {
        await using var host = await Host.Open(); using var editor = await Editor(host); await editor.ReloadAsync();
        var draft = editor.Draft!;
        draft.AddItem("/sources"); draft.SelectVariant("/sources/3/backend", "simulated");
        draft.SetValue("/sources/3/backend/deviceType", draft.ParseScalar(draft.Field("/sources/3/backend/deviceType").Schema, type));
        draft.SetValue("/sources/3/label", JsonSerializer.SerializeToElement("Shared simulated " + type));
        var source = draft.Field("/sources/3/id").Value!.Value.GetGuid();
        var ids = new List<Guid>();
        for (int index = 3; index < 5; index++) {
            draft.AddItem("/outputs"); draft.SelectVariant($"/outputs/{index}/device", "proxy");
            Assert.Equal("focuser", draft.Field($"/outputs/{index}/device/deviceType").Value!.Value.GetString());
            draft.SetValue($"/outputs/{index}/device/deviceType", draft.ParseScalar(draft.Field($"/outputs/{index}/device/deviceType").Schema, type));
            Assert.Throws<FormatException>(() => draft.ParseScalar(draft.Field($"/outputs/{index}/device/deviceType").Schema, "camera"));
            draft.SetValue($"/outputs/{index}/device/source", JsonSerializer.SerializeToElement(source));
            draft.SetValue($"/outputs/{index}/label", JsonSerializer.SerializeToElement($"Shared {type} {index}"));
            draft.SetValue($"/outputs/{index}/number", JsonSerializer.SerializeToElement(index == 3 ? 4 : 7));
            ids.Add(draft.Field($"/outputs/{index}/id").Value!.Value.GetGuid());
        }
        // Schema choices guide setup; the engine still authorizes the candidate.
        draft.SetValue("/outputs/3/device/deviceType", JsonSerializer.SerializeToElement("camera")); editor.Changed();
        Assert.False(await editor.ReviewAsync()); Assert.NotEmpty(editor.Errors.EnumerateArray());
        draft.SetValue("/outputs/3/device/deviceType", JsonSerializer.SerializeToElement(type == "focuser" ? "rotator" : "focuser")); editor.Changed();
        Assert.False(await editor.ReviewAsync()); Assert.NotEmpty(editor.Errors.EnumerateArray());
        draft.SetValue("/outputs/3/device/deviceType", JsonSerializer.SerializeToElement(type)); editor.Changed();
        Assert.True(await editor.ReviewAsync());
        for (int index = 0; index < 3; index++) Assert.Equal(0, (await editor.SourceStatusAsync(Guid.Parse(host.Config["sources"]![index]!["id"]!.GetValue<string>()))).GetProperty("leaseCount").GetInt32());
        await editor.ApplyAsync(); await editor.ReloadAsync();
        Assert.Equal(0, (await editor.SourceStatusAsync(source)).GetProperty("leaseCount").GetInt32());
        Assert.Equal(ids[0], editor.Draft!.Field("/outputs/3/id").Value!.Value.GetGuid());
        Assert.Equal(ids[1], editor.Draft.Field("/outputs/4/id").Value!.Value.GetGuid());
        HubSelection Selection(int index) => new() { ConfigPath = host.ConfigPath, InstanceId = host.Selection(0,"switch").InstanceId,
            OutputId = ids[index], DeviceType = type, Label = $"Shared {type} {index + 3}", Simulated = true };
        HubDevice Device(int index) => type == "focuser" ? new HubFocuserDevice(Selection(index),host.Executable,host.Workers)
            : new HubRotatorDevice(Selection(index),host.Executable,host.Workers);
        using var first = Device(0);
        using var second = Device(1);
        await first.Connect(CancellationToken.None); await second.Connect(CancellationToken.None);
        await Eventually(async () => (await editor.SourceStatusAsync(source)).GetProperty("leaseCount").GetInt32() == 2);
        if (first is HubFocuserDevice focuser) {
            await focuser.Move(50100,CancellationToken.None,0); Assert.Equal(50100,((HubFocuserDevice)second).Position);
        } else {
            var rotator = (HubRotatorDevice)first; rotator.Sync(42.5f);
            Assert.True(await rotator.Move(-721.5f,CancellationToken.None));
            Assert.Equal(41.0f,((HubRotatorDevice)second).Position);
            Assert.Equal(358.5f,((HubRotatorDevice)second).MechanicalPosition);
        }
        first.Disconnect(); Assert.True(second.Connected); second.Disconnect();
        await Eventually(async () => (await editor.SourceStatusAsync(source)).GetProperty("leaseCount").GetInt32() == 0);
    }
    [Fact]
    public async Task NativeEditorReviewsAndAppliesWithoutOpeningEquipment()
    {
        await using var host = await Host.Open(); using var editor = await Editor(host); await editor.ReloadAsync();
        Assert.Equal(HubEditorState.Editing, editor.State); var old = editor.Draft!.Revision;
        editor.Draft.SetValue("/outputs/0/label", JsonSerializer.SerializeToElement("Native edited")); editor.Changed();
        Assert.True(await editor.ReviewAsync()); Assert.Equal(HubEditorState.Reviewed, editor.State);
        Assert.Contains("Native edited", editor.Draft.Preview());
        for (int i = 0; i < 3; i++) Assert.Equal(0, (await editor.SourceStatusAsync(Guid.Parse(host.Config["sources"]![i]!["id"]!.GetValue<string>()))).GetProperty("leaseCount").GetInt32());
        var applied = await editor.ApplyAsync(); Assert.True(applied.GetProperty("ready").GetBoolean());
        Assert.Equal(HubEditorState.Uncertain, editor.State); // reconciliation is required, even after a successful response
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.ApplyAsync());
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.ReviewAsync()); Assert.Equal(HubEditorState.Uncertain, editor.State);
        await editor.ReloadAsync(); Assert.Equal(HubEditorState.Editing, editor.State); Assert.NotEqual(old, editor.Draft!.Revision);
        Assert.Equal("Native edited", editor.Draft.Field("/outputs/0/label").Value!.Value.GetString());
        using var device = host.Switch(); await device.Connect(CancellationToken.None); Assert.Contains("Native edited", device.Name);
    }
    [Fact]
    public async Task NativeEditorRejectsChangedDraftsAndCompetingSavedRevisions()
    {
        await using var host = await Host.Open(); using var first = await Editor(host); using var second = await Editor(host);
        await first.ReloadAsync(); await second.ReloadAsync();
        Assert.True(await first.ReviewAsync());
        first.Draft!.SetValue("/outputs/0/label", JsonSerializer.SerializeToElement("First editor"));
        await Assert.ThrowsAsync<InvalidOperationException>(() => first.ApplyAsync());
        Assert.Equal(first.Draft.Revision, (await host.Command(new { op = "getConfig" })).GetProperty("revision").GetGuid());
        Assert.True(await first.ReviewAsync());
        second.Draft!.SetValue("/outputs/0/label", JsonSerializer.SerializeToElement("Second editor")); second.Changed(); Assert.True(await second.ReviewAsync());
        await first.ApplyAsync();
        Assert.Equal("revisionConflict", (await Assert.ThrowsAsync<HubException>(() => second.ApplyAsync())).Remote!.Code);
        Assert.Equal(HubEditorState.Uncertain, second.State);
        await Assert.ThrowsAsync<InvalidOperationException>(() => second.ReviewAsync()); Assert.Equal(HubEditorState.Uncertain, second.State);
        await second.ReloadAsync(); Assert.Equal("First editor", second.Draft!.Field("/outputs/0/label").Value!.Value.GetString());
    }
    [Fact]
    public async Task NativeEditorReportsFieldErrorsAndCannotDisconnectNinaToApply()
    {
        await using var host = await Host.Open(); using var editor = await Editor(host); await editor.ReloadAsync();
        editor.Draft!.SetValue("/sources/1/polling/pollSeconds", JsonSerializer.SerializeToElement(91.0)); editor.Changed();
        Assert.False(await editor.ReviewAsync()); Assert.NotEmpty(editor.Errors.EnumerateArray());
        Assert.All(editor.Errors.EnumerateArray(), e => Assert.False(string.IsNullOrEmpty(e.GetProperty("path").GetString())));
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.ApplyAsync());
        await editor.ReloadAsync(); using var device = host.Switch(); await device.Connect(CancellationToken.None);
        var old = editor.Draft!.Revision;
        editor.Draft.SetValue("/outputs/0/label", JsonSerializer.SerializeToElement("Safe pending edit")); editor.Changed();
        Assert.True(await editor.ReviewAsync());
        Assert.Equal("connected", (await Assert.ThrowsAsync<HubException>(() => editor.ApplyAsync())).Remote!.Code);
        Assert.True(device.Connected); Assert.Equal(HubEditorState.Editing, editor.State);
        Assert.Equal(old, (await host.Command(new { op = "getConfig" })).GetProperty("revision").GetGuid());
        device.Disconnect(); await Eventually(async () => (await host.Status(0)).GetProperty("leaseCount").GetInt32() == 0);
        Assert.True(await editor.ReviewAsync()); await editor.ApplyAsync(); await editor.ReloadAsync();
        Assert.Equal("Safe pending edit", editor.Draft!.Field("/outputs/0/label").Value!.Value.GetString());
    }
    [Fact]
    public async Task NativeEditorDoesNotReplayAnApplyAfterACommittedReplyIsLost()
    {
        var saved = JsonNode.Parse(HubDraftTests.Configuration().GetRawText())!.AsObject(); int dispatched = 0;
        using var editor = new HubEditorSession(Guid.Parse(saved["instanceId"]!.GetValue<string>()), (command, _) => {
            object result;
            switch (command.GetProperty("op").GetString()) {
                case "describeConfig": result = HubDraftTests.Description(); break;
                case "getConfig": result = saved; break;
                case "hostStatus": result = new { phase = "ready", configurationRevision = saved["revision"]!.GetValue<string>() }; break;
                case "validateConfig": result = new { valid = true, errors = Array.Empty<object>() }; break;
                case "applyConfig":
                    dispatched++; saved = JsonNode.Parse(command.GetProperty("candidate").GetRawText())!.AsObject(); saved["revision"] = Guid.NewGuid().ToString();
                    throw new HubException(HubFailure.Uncertain);
                default: throw new InvalidOperationException("Unexpected editor operation");
            }
            return Task.FromResult(JsonSerializer.SerializeToElement(result));
        }, () => { });
        await editor.ReloadAsync(); editor.Draft!.SetValue("/outputs/0/label", JsonSerializer.SerializeToElement("Committed once")); editor.Changed();
        Assert.True(await editor.ReviewAsync()); await Assert.ThrowsAsync<HubException>(() => editor.ApplyAsync());
        Assert.Equal(HubEditorState.Uncertain, editor.State);
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.ApplyAsync());
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.ReviewAsync()); Assert.Equal(1, dispatched);
        await editor.ReloadAsync(); Assert.Equal("Committed once", editor.Draft!.Field("/outputs/0/label").Value!.Value.GetString());
        Assert.False(editor.Draft.Dirty); Assert.Equal(HubEditorState.Editing, editor.State); Assert.Equal(1, dispatched);
    }
    [Fact]
    public async Task NativeEditorDisposeCancelsQueuedReviewAndCannotReviveState()
    {
        var entered = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously); int closed = 0;
        var saved = HubDraftTests.Configuration();
        using var editor = new HubEditorSession(saved.GetProperty("instanceId").GetGuid(), async (command, cancellation) => {
            switch (command.GetProperty("op").GetString()) {
                case "describeConfig": return HubDraftTests.Description();
                case "getConfig": return saved;
                case "hostStatus": return JsonSerializer.SerializeToElement(new { phase = "ready", configurationRevision = saved.GetProperty("revision").GetGuid() });
                case "validateConfig": entered.TrySetResult(true); await Task.Delay(Timeout.Infinite, cancellation); throw new Exception("Unreachable");
                default: throw new InvalidOperationException("Unexpected editor operation");
            }
        }, () => Interlocked.Increment(ref closed));
        await editor.ReloadAsync(); var first = editor.ReviewAsync(); await entered.Task.WaitAsync(TimeSpan.FromSeconds(5));
        var second = editor.ReviewAsync(); editor.Dispose(); editor.Dispose();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => first);
        await Assert.ThrowsAsync<ObjectDisposedException>(() => second);
        Assert.Equal(HubEditorState.Disposed, editor.State); Assert.Equal(1, closed);
        await Assert.ThrowsAsync<ObjectDisposedException>(() => editor.ReloadAsync());
    }
    [Theory]
    [InlineData("{}")]
    [InlineData("{\"valid\":true,\"errors\":[{}]}")]
    [InlineData("{\"valid\":true,\"errors\":[{\"path\":\"sources\",\"message\":\"invalid\"}]}")]
    public async Task NativeEditorMalformedReviewRequiresReload(string validation)
    {
        var saved = HubDraftTests.Configuration(); using var json = JsonDocument.Parse(validation);
        using var editor = new HubEditorSession(saved.GetProperty("instanceId").GetGuid(), (command, _) => Task.FromResult(command.GetProperty("op").GetString() switch {
            "describeConfig" => HubDraftTests.Description(), "getConfig" => saved,
            "hostStatus" => JsonSerializer.SerializeToElement(new { phase = "ready", configurationRevision = saved.GetProperty("revision").GetGuid() }),
            "validateConfig" => json.RootElement.Clone(), _ => throw new Exception("Unexpected operation")
        }), () => { });
        await editor.ReloadAsync(); Assert.Equal(HubFailure.Protocol, (await Assert.ThrowsAsync<HubException>(() => editor.ReviewAsync())).Failure);
        Assert.Equal(HubEditorState.Uncertain, editor.State);
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.ApplyAsync());
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.ReviewAsync());
        Assert.Equal(HubEditorState.Uncertain, editor.State);
    }
    [Fact]
    public async Task NativeEditorHealthTransportFailureRevokesAnEarlierReview()
    {
        var saved = HubDraftTests.Configuration(); int writes = 0;
        using var editor = new HubEditorSession(saved.GetProperty("instanceId").GetGuid(), (command, _) => {
            var op = command.GetProperty("op").GetString();
            if (op == "sourceStatus") throw new HubException(HubFailure.Disconnected);
            if (op == "applyConfig") { writes++; throw new Exception("Unreachable"); }
            return Task.FromResult(op switch {
                "describeConfig" => HubDraftTests.Description(), "getConfig" => saved,
                "hostStatus" => JsonSerializer.SerializeToElement(new { phase = "ready", configurationRevision = saved.GetProperty("revision").GetGuid() }),
                "validateConfig" => JsonSerializer.SerializeToElement(new { valid = true, errors = Array.Empty<object>() }),
                _ => throw new Exception("Unexpected operation")
            });
        }, () => { });
        await editor.ReloadAsync(); Assert.True(await editor.ReviewAsync());
        await Assert.ThrowsAsync<HubException>(() => editor.SourceStatusAsync(saved.GetProperty("sources")[0].GetProperty("id").GetGuid()));
        Assert.Equal(HubEditorState.Uncertain, editor.State);
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.ApplyAsync()); Assert.Equal(0, writes);
        await editor.ReloadAsync(); Assert.Equal(HubEditorState.Editing, editor.State);
    }
}
