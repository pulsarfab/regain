using System.Text.Json;
using System.Text.Json.Nodes;
using Regain.Hub;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class HubCameraGroupContractTests
{
    private static JsonElement Element(JsonNode node) => JsonSerializer.SerializeToElement(node);
    internal static JsonObject Configuration()
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);
        while (directory is not null && !File.Exists(Path.Combine(directory.FullName, "Cargo.toml"))) directory = directory.Parent;
        return JsonNode.Parse(File.ReadAllText(Path.Combine(directory!.FullName, "crates", "regain-hub", "examples", "paired-cameras.json")))!.AsObject();
    }
    [Fact]
    public void SharedCameraGroupDraftKeepsIdentityReferencesAndExplicitPolicies()
    {
        var description = JsonNode.Parse(File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "hub-config.json")))!;
        var saved = Configuration();
        var gated = new HubConfigurationDraft(Element(description), Element(saved));
        Assert.False(gated.Field("/cameraGroups").Enabled);
        description["capabilities"] = new JsonArray("cameraGroups");
        var draft = new HubConfigurationDraft(Element(description), Element(saved));
        Assert.True(draft.Field("/cameraGroups").Enabled);
        Assert.Throws<InvalidOperationException>(() => draft.SetValue("/cameraGroups/0/id", JsonSerializer.SerializeToElement(Guid.NewGuid())));
        var source = saved["sources"]![0]!["id"]!.GetValue<Guid>();
        draft.SetValue("/cameraGroups/0/members/0", JsonSerializer.SerializeToElement(source));
        draft.SetValue("/cameraGroups/0/failurePolicy", JsonSerializer.SerializeToElement("abortStarted"));
        draft.SetValue("/cameraGroups/0/cancellationPolicy", JsonSerializer.SerializeToElement("leaveRunning"));
        Assert.Contains("abortStarted", draft.Preview()); Assert.Contains("leaveRunning", draft.Preview());
        draft.AddItem("/cameraGroups");
        Assert.NotEqual(Guid.Empty, draft.Field("/cameraGroups/1/id").Value!.Value.GetGuid());
        Assert.Equal(300, draft.Field("/cameraGroups/1/timeoutSeconds").Value!.Value.GetDouble());
        Assert.Equal("cameraGroupImage", description["coordination"]!["cameraGroups"]!["imageOperation"]!.GetValue<string>());
    }
    [Fact]
    public async Task ColdIdentityReaderAdmitsOptionalCameraGroupsAndRejectsWrongTypes()
    {
        var directory = Path.Combine(Path.GetTempPath(), "Regain.CameraGroup.Contract." + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try {
            var path = Path.Combine(directory, "hub.json"); var config = Configuration();
            // Creation replies must be empty; reading remains inert and accepts
            // the saved equipment config without launching a host or driver.
            var empty = config.DeepClone(); empty["sources"] = new JsonArray(); empty["outputs"] = new JsonArray(); empty["cameraGroups"] = new JsonArray();
            var initialization = new HubInitialization((_, _) => Task.FromResult(JsonSerializer.SerializeToUtf8Bytes(empty)));
            await initialization.CreateAsync(path);
            await File.WriteAllTextAsync(path, config.ToJsonString());
            await initialization.ReadAsync(); Assert.Equal(HubInitializationState.Existing, initialization.State);
            Assert.Equal(config["instanceId"]!.GetValue<Guid>(), initialization.InstanceId);
            config["cameraGroups"] = new JsonObject(); await File.WriteAllTextAsync(path, config.ToJsonString());
            var error = await Assert.ThrowsAsync<HubException>(() => initialization.ReadAsync());
            Assert.Equal(HubFailure.Protocol, error.Failure);
        } finally {Directory.Delete(directory, true);}
    }
}
