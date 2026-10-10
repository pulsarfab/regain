using System.Text.Json;
using System.Text.Json.Nodes;
using Regain.Hub;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class HubAdoptionTests
{
    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task ScopedCatalogFreezesItsInterfaceAndRejectsWrongScopeEcho(bool wrongEcho)
    {
        var saved = HubDraftTests.Configuration(); var revision = saved.GetProperty("revision").GetGuid(); int queries = 0;
        using var editor = new HubEditorSession(saved.GetProperty("instanceId").GetGuid(), (command, _) => {
            switch (command.GetProperty("op").GetString()) {
                case "describeConfig": return Task.FromResult(HubDraftTests.Description());
                case "getConfig": return Task.FromResult(saved);
                case "hostStatus": return Task.FromResult(JsonSerializer.SerializeToElement(new { phase = "ready", configurationRevision = revision }));
                case "discoverAlpaca":
                    queries++; Assert.Equal(7u, command.GetProperty("scopeId").GetUInt32());
                    var catalog = JsonNode.Parse(Catalog(revision).GetRawText())!;
                    catalog["baseUrl"] = "http://[fe80::42]:11111/prefix"; catalog["scopeId"] = wrongEcho ? 8 : 7;
                    return Task.FromResult(JsonSerializer.SerializeToElement(catalog));
                default: throw new Exception("Unexpected equipment/configuration I/O");
            }
        }, () => { });
        await editor.ReloadAsync();
        foreach (var pair in new (string, uint?)[] { ("http://[fe80::42]", null), ("http://[fe80::42]", 0),
            ("http://localhost", 7), ("http://[::1]", 7), ("http://[fe80::42%7]", 7), ("http://[fe80::42%257]", 7) })
            await Assert.ThrowsAsync<InvalidOperationException>(() => editor.DiscoverAlpacaScopedAsync(pair.Item1, pair.Item2));
        Assert.Equal(0, queries);
        if (wrongEcho) {
            await Assert.ThrowsAsync<HubException>(() => editor.DiscoverAlpacaScopedAsync("http://[fe80::42]:11111/prefix", 7));
            Assert.Null(editor.LastDiscovery); Assert.Equal(HubEditorState.Uncertain, editor.State);
        } else {
            await editor.DiscoverAlpacaScopedAsync("http://[fe80::42]:11111/prefix", 7, "frozen-reference");
            var id = editor.AddDiscoveredAlpacaSource(0);
            var backend = editor.Draft!.Candidate.GetProperty("sources").EnumerateArray().Single(s => s.GetProperty("id").GetGuid() == id).GetProperty("backend");
            Assert.Equal(7u, backend.GetProperty("scopeId").GetUInt32());
            Assert.Equal("frozen-reference", backend.GetProperty("credentialReference").GetString());
        }
        Assert.Equal(1, queries);
    }
    [Fact]
    public void DifferentInterfaceScopesPermitDifferentDevicesButNeverAliasTheSamePin()
    {
        var draft = new HubConfigurationDraft(HubDraftTests.Description(), HubDraftTests.Configuration());
        var catalog = JsonNode.Parse(Catalog(draft.Revision).GetRawText())!;
        catalog["baseUrl"] = "http://[fe80::42]:11111"; catalog["scopeId"] = 7;
        draft.AddDiscoveredAlpacaSource(JsonSerializer.SerializeToElement(catalog), 0, null);
        catalog["scopeId"] = 8;
        Assert.Throws<InvalidOperationException>(() => draft.AddDiscoveredAlpacaSource(JsonSerializer.SerializeToElement(catalog), 0, null));
        catalog["devices"]![0]!["uniqueId"] = "different camera";
        draft.AddDiscoveredAlpacaSource(JsonSerializer.SerializeToElement(catalog), 0, null);
        draft.RemoveOptional("/sources/4/backend/uniqueId");
        Assert.Throws<InvalidOperationException>(() => draft.AddDiscoveredAlpacaSource(JsonSerializer.SerializeToElement(catalog), 0, null));
    }
    private static JsonElement Catalog(Guid revision) => JsonSerializer.SerializeToElement(new {
        configurationRevision = revision, baseUrl = "http://localhost:11111/prefix",
        devices = new object[] {
            new { name = "[SIMULATION] " + string.Concat(Enumerable.Repeat("🔭", 230)), reportedDeviceType = "Camera", supportedDeviceType = "camera", number = uint.MaxValue, uniqueId = "camera unit 42" },
            new { name = "[SIMULATION] mount", reportedDeviceType = "Telescope", supportedDeviceType = (string?)null, number = 9u, uniqueId = "mount-99" }
        }
    });
    [Fact]
    public async Task AdoptionFreezesTheQueriedIdentityAndCredentialsWithoutSavingOrNetworkIo()
    {
        var saved = HubDraftTests.Configuration(); var revision = saved.GetProperty("revision").GetGuid(); int queries = 0;
        using var editor = new HubEditorSession(saved.GetProperty("instanceId").GetGuid(), (command, _) => {
            return Task.FromResult(command.GetProperty("op").GetString() switch {
                "describeConfig" => HubDraftTests.Description(), "getConfig" => saved,
                "hostStatus" => JsonSerializer.SerializeToElement(new { phase = "ready", configurationRevision = revision }),
                "validateConfig" => JsonSerializer.SerializeToElement(new { valid = true, errors = Array.Empty<object>() }),
                "discoverAlpaca" => Query(), _ => throw new Exception("Unexpected configuration or equipment I/O")
            });
            JsonElement Query() { queries++; return Catalog(revision); }
        }, () => { });
        await editor.ReloadAsync(); await editor.DiscoverAlpacaAsync("http://localhost:11111/prefix/", "frozen-reference");
        Assert.True(await editor.ReviewAsync());
        var before = editor.Draft!.Candidate.GetRawText();
        foreach (var unsupported in new[] { -1, 1, 2 }) Assert.Throws<InvalidOperationException>(() => editor.AddDiscoveredAlpacaSource(unsupported));
        Assert.Equal(before, editor.Draft.Candidate.GetRawText()); Assert.Equal(HubEditorState.Reviewed, editor.State);
        var id = editor.AddDiscoveredAlpacaSource(0); Assert.NotEqual(Guid.Empty, id);
        var source = editor.Draft.Candidate.GetProperty("sources").EnumerateArray().Single(s => s.GetProperty("id").GetGuid() == id);
        Assert.Equal(200, source.GetProperty("label").GetString()!.EnumerateRunes().Count());
        var backend = source.GetProperty("backend");
        Assert.Equal("camera unit 42", backend.GetProperty("uniqueId").GetString());
        Assert.Equal(uint.MaxValue, backend.GetProperty("deviceNumber").GetUInt32());
        Assert.Equal("frozen-reference", backend.GetProperty("credentialReference").GetString());
        Assert.Equal("externallyManaged", backend.GetProperty("connectionPolicy").GetString());
        Assert.Equal(30d, source.GetProperty("polling").GetProperty("pollSeconds").GetDouble());
        Assert.Equal(HubEditorState.Editing, editor.State); Assert.True(editor.Draft.Dirty);
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.ApplyAsync());
        Assert.Equal(saved.GetRawText(), editor.SavedConfiguration!.Value.GetRawText()); Assert.Equal(1, queries);
        before = editor.Draft.Candidate.GetRawText();
        Assert.Throws<InvalidOperationException>(() => editor.AddDiscoveredAlpacaSource(0)); Assert.Equal(before, editor.Draft.Candidate.GetRawText());
        await editor.ReloadAsync(); Assert.Throws<InvalidOperationException>(() => editor.AddDiscoveredAlpacaSource(0));
    }
    [Theory]
    [InlineData("00112233445566778899AABBCCDDEEFF", true)]
    [InlineData("{00112233-4455-6677-8899-aabbccddeeff}", true)]
    [InlineData("urn:uuid:00112233-4455-6677-8899-aabbccddeeff", true)]
    [InlineData("00112233-4455-6677-8899-aabbccddeeff ", false)]
    [InlineData("urn:uuid:00112233445566778899aabbccddeeff", false)]
    [InlineData("{00112233445566778899aabbccddeeff}", false)]
    [InlineData("{0x00112233,0x4455,0x6677,{0x88,0x99,0xaa,0xbb,0xcc,0xdd,0xee,0xff}}", false)]
    public void CatalogAdoptionNormalizesOnlyTheCoreUuidFormsAndPreservesOpaqueStrings(string identity, bool duplicate)
    {
        var draft = new HubConfigurationDraft(HubDraftTests.Description(), HubDraftTests.Configuration());
        var catalog = JsonNode.Parse(Catalog(draft.Revision).GetRawText())!;
        catalog["devices"]![0]!["uniqueId"] = "00112233-4455-6677-8899-aabbccddeeff";
        draft.AddDiscoveredAlpacaSource(JsonSerializer.SerializeToElement(catalog), 0, null);
        catalog["devices"]![0]!["uniqueId"] = identity;
        catalog["devices"]![0]!["number"] = 1;
        if (duplicate) Assert.Throws<InvalidOperationException>(() => draft.AddDiscoveredAlpacaSource(JsonSerializer.SerializeToElement(catalog), 0, null));
        else {
            draft.AddDiscoveredAlpacaSource(JsonSerializer.SerializeToElement(catalog), 0, null);
            Assert.Equal(identity, draft.Candidate.GetProperty("sources")[4].GetProperty("backend").GetProperty("uniqueId").GetString());
        }
    }
    [Theory]
    [InlineData("capacity")]
    [InlineData("capability")]
    [InlineData("revision")]
    [InlineData("address")]
    [InlineData("pin-alias")]
    public void RejectedAdoptionPreservesTheWholeDraft(string fault)
    {
        var description = JsonNode.Parse(HubDraftTests.Description().GetRawText())!;
        if (fault == "capability") description["capabilities"] = new JsonArray();
        var draft = new HubConfigurationDraft(JsonSerializer.SerializeToElement(description), HubDraftTests.Configuration());
        var catalog = Catalog(fault == "revision" ? Guid.NewGuid() : draft.Revision);
        if (fault == "capacity") while (draft.Candidate.GetProperty("sources").GetArrayLength() < 256) draft.AddItem("/sources");
        if (fault is "address" or "pin-alias") {
            draft.AddDiscoveredAlpacaSource(catalog, 0, null);
            if (fault == "address") draft.RemoveOptional("/sources/3/backend/uniqueId");
            else draft.SetValue("/sources/3/backend/baseUrl", JsonSerializer.SerializeToElement("http://alias.example/"));
        }
        var before = draft.Candidate.GetRawText(); var version = draft.Version;
        Assert.Throws<InvalidOperationException>(() => draft.AddDiscoveredAlpacaSource(catalog, 0, null));
        Assert.Equal(before, draft.Candidate.GetRawText()); Assert.Equal(version, draft.Version);
    }
}
