using System.Text.Json;
using System.Text.Json.Nodes;
using Regain.Hub;
using Regain.Hub.ASCOM;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class HubLocalDiscoveryTests
{
    [Fact]
    public void RealRegistryApiUsesOnlyPrivateFixtureRoots() => Regain.TestFixtures.HubLocalRegistryFixture.Run();
    private static JsonElement Json(object value) => JsonSerializer.SerializeToElement(value);
    private static JsonElement Description() {
        var node = JsonNode.Parse(HubDraftTests.Description().GetRawText())!;
        foreach (var capability in new[] { "comSources", "comX64Sources", "comX86Sources" }) node["capabilities"]!.AsArray().Add(capability);
        return Json(node);
    }
    private static JsonElement Target(bool com) => com ? Json(new { kind = "com", deviceType = "focuser", bitness = "x64" }) : Json(new { kind = "native", device = "eaf" });
    private static JsonElement Catalog(Guid revision, bool com) => Json(new {
        configurationRevision = revision, target = Target(com), simulated = !com, ignoredEntries = 0, incomplete = false,
        entries = new[] { new { name = com ? "[FIXTURE] focuser" : "[SIMULATION] EAF", backend = com ?
            Json(new { kind = "com", progId = "Fixture.Focuser", deviceType = "focuser", bitness = "x64", connectionPolicy = "externallyManaged" }) :
            Json(new { kind = "native", device = "eaf", identity = "0102030405060708", filterWheel = (object?)null, camera = (object?)null }),
            registeredClass = com ? Guid.NewGuid() : (Guid?)null, blockedReason = (string?)null } }
    });
    private static HubEditorSession Editor(Func<JsonElement, JsonElement> query) {
        var saved = HubDraftTests.Configuration();
        return new(saved.GetProperty("instanceId").GetGuid(), (command, _) => Task.FromResult(command.GetProperty("op").GetString() switch {
            "describeConfig" => Description(), "getConfig" => saved,
            "hostStatus" => Json(new { phase = "ready", configurationRevision = saved.GetProperty("revision").GetGuid() }),
            "validateConfig" => Json(new { valid = true, errors = Array.Empty<object>() }), "discoverLocal" => query(command),
            _ => throw new Exception("Unexpected configuration/equipment I/O")
        }), () => { });
    }
    [Theory]
    [InlineData(false)] [InlineData(true)]
    public async Task CatalogAdoptionChangesOnlyTheDraftAndRejectsDuplicatePhysicalDevices(bool com) {
        var calls = 0;
        using var editor = Editor(command => { calls++; Assert.Equal(Target(com).GetRawText(), command.GetProperty("target").GetRawText()); return Catalog(command.GetProperty("expectedRevision").GetGuid(), com); });
        await editor.ReloadAsync(); await editor.DiscoverLocalAsync(Target(com)); Assert.True(await editor.ReviewAsync());
        var before = editor.Draft!.Candidate.GetRawText();
        Assert.Throws<InvalidOperationException>(() => editor.AddDiscoveredLocalSource(-1));
        Assert.Equal(before, editor.Draft.Candidate.GetRawText()); Assert.Equal(HubEditorState.Reviewed, editor.State);
        var id = editor.AddDiscoveredLocalSource(0); Assert.NotEqual(Guid.Empty, id);
        Assert.Equal(HubEditorState.Editing, editor.State); Assert.Equal(3, editor.SavedConfiguration!.Value.GetProperty("sources").GetArrayLength());
        Assert.Equal(4, editor.Draft.Candidate.GetProperty("sources").GetArrayLength());
        var source = editor.Draft.Candidate.GetProperty("sources")[3];
        Assert.Equal(30d, source.GetProperty("polling").GetProperty("pollSeconds").GetDouble());
        Assert.Throws<InvalidOperationException>(() => editor.AddDiscoveredLocalSource(0));
        await editor.ReloadAsync(); Assert.Null(editor.LastLocalDiscovery);
        Assert.Throws<InvalidOperationException>(() => editor.AddDiscoveredLocalSource(0)); Assert.Equal(1, calls);
    }
    [Theory]
    [InlineData("revision")] [InlineData("target")] [InlineData("duplicate")] [InlineData("serial")]
    [InlineData("name")] [InlineData("incomplete")] [InlineData("backend")] [InlineData("registration")]
    public async Task InvalidCatalogNeverAuthorizesAdoption(string fault) {
        using var editor = Editor(command => {
            var node = JsonNode.Parse(Catalog(command.GetProperty("expectedRevision").GetGuid(), false).GetRawText())!;
            var entry = node["entries"]![0]!;
            switch (fault) {
                case "revision": node["configurationRevision"] = Guid.NewGuid(); break;
                case "target": node["target"]!["device"] = "fc3"; break;
                case "duplicate": node["entries"]!.AsArray().Add(entry.DeepClone()); break;
                case "serial": entry["backend"]!["identity"] = "\u0085invalid"; break;
                case "name": entry["name"] = " "; break;
                case "incomplete": node["ignoredEntries"] = 1; break;
                case "backend": entry["backend"]!["device"] = "fc3"; break;
                case "registration": entry["registeredClass"] = Guid.NewGuid(); break;
            }
            return Json(node);
        });
        await editor.ReloadAsync(); await Assert.ThrowsAsync<HubException>(() => editor.DiscoverLocalAsync(Target(false)));
        Assert.Equal(HubEditorState.Uncertain, editor.State); Assert.Null(editor.LastLocalDiscovery); Assert.False(editor.Draft!.Dirty);
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.DiscoverLocalAsync(Target(false)));
    }
    [Theory]
    [InlineData("missingRegistration")] [InlineData("selfProxy")]
    public async Task UnusableComRegistrationIsVisibleButCannotBecomeASource(string reason) {
        using var editor = Editor(command => {
            var node = JsonNode.Parse(Catalog(command.GetProperty("expectedRevision").GetGuid(), true).GetRawText())!;
            node["entries"]![0]!["blockedReason"] = reason;
            if (reason == "missingRegistration") node["entries"]![0]!["registeredClass"] = null;
            return Json(node);
        });
        await editor.ReloadAsync(); var catalog = await editor.DiscoverLocalAsync(Target(true)); Assert.Single(catalog.GetProperty("entries").EnumerateArray());
        var before = editor.Draft!.Candidate.GetRawText(); Assert.Throws<InvalidOperationException>(() => editor.AddDiscoveredLocalSource(0));
        Assert.Equal(before, editor.Draft.Candidate.GetRawText());
    }
    private sealed class Registry : IHubDriverRegistry {
        public IEnumerable<string> Names = new[] { "Fixture.Focuser" }; public bool Fail; public Guid? Clsid = Guid.NewGuid(); public string? Name = "[FIXTURE] focuser";
        public IEnumerable<string> Drivers(string type) { Assert.Contains(type, HubSelection.Types); foreach (var name in Names) { yield return name; } if (Fail) throw new IOException(); }
        public string? Description(string type, string progId) => Name;
        public Guid? ClassId(string progId) => Clsid;
    }
    [Fact]
    public void ReadOnlyComCollectorSupportsEveryClassAndBoundsPartialRegistryData() {
        foreach (var type in HubSelection.Types) Assert.Single(HubDriverCatalog.Read(new Registry(), type).Entries);
        var registry = new Registry { Names = new[] { "Fixture.Focuser", "fixture.focuser", "bad\\path", "", "Other.Focuser" }, Name = "\u0085bad", Clsid = Guid.Empty, Fail = true };
        var catalog = HubDriverCatalog.Read(registry, "focuser"); Assert.True(catalog.Incomplete); Assert.Equal(2, catalog.Entries.Count);
        Assert.All(catalog.Entries, entry => { Assert.Equal(entry.ProgId, entry.Name); Assert.Null(entry.ClassId); });
        registry = new Registry { Names = Enumerable.Range(0, 5000).Select(i => "Fixture." + i), Name = string.Concat(Enumerable.Repeat("🔭", 150)) };
        catalog = HubDriverCatalog.Read(registry, "camera"); Assert.True(catalog.Incomplete); Assert.Equal(256, catalog.Entries.Count);
        Assert.All(catalog.Entries, entry => { Assert.Equal(200, entry.Name.Length); Assert.False(char.IsHighSurrogate(entry.Name.Last())); });
        Assert.Throws<ArgumentException>(() => HubDriverCatalog.Read(registry, "telescope"));
    }
}
