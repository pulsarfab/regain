using System.Text.Json;
using System.Text.Json.Nodes;
using System.Windows;
using System.Windows.Controls;
using Regain.Hub;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class HubDraftTests
{
    private static JsonElement Json(object value) => JsonSerializer.SerializeToElement(value);
    internal static JsonElement Description()
    {
        var node = JsonNode.Parse(File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "hub-config.json")))!;
        node["capabilities"] = new JsonArray("nativeSources", "alpacaSources", "virtualSources", "simulation", "writeReadout");
        return Json(node);
    }
    internal static JsonElement Configuration()
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);
        while (directory is not null && !File.Exists(Path.Combine(directory.FullName, "Cargo.toml"))) directory = directory.Parent;
        using var json = JsonDocument.Parse(File.ReadAllText(Path.Combine(directory!.FullName, "crates", "regain-hub", "examples", "simulated-observatory.json")));
        return json.RootElement.Clone();
    }
    [Fact]
    public void DraftPreservesSavedIdentitiesAndCreatesNewOnesFromTheSchema()
    {
        var saved = Configuration(); var draft = new HubConfigurationDraft(Description(), saved);
        Assert.False(draft.Dirty);
        Assert.Throws<InvalidOperationException>(() => draft.SetValue("/revision", Json(Guid.NewGuid())));
        Assert.Throws<InvalidOperationException>(() => draft.SetValue("/sources/0/id", Json(Guid.NewGuid())));
        Assert.Throws<InvalidOperationException>(() => draft.SetValue("/outputs/0/number", Json(5)));
        Assert.Throws<InvalidOperationException>(() => draft.SetValue("/outputs/0", Json(new { id = Guid.NewGuid() })));
        Assert.Throws<InvalidOperationException>(() => draft.SetValue("/outputs", Json(Array.Empty<object>())));
        draft.SetValue("/outputs/0/label", Json("Renamed"));
        Assert.Equal("Simulation controls", saved.GetProperty("outputs")[0].GetProperty("label").GetString());
        Assert.Equal("Renamed", draft.Candidate.GetProperty("outputs")[0].GetProperty("label").GetString());
        draft.AddItem("/outputs");
        Assert.False(draft.Field("/outputs/3/number").ReadOnly); draft.SetValue("/outputs/3/number", Json(8));
        var identity = draft.Field("/outputs/3/id").Value!.Value.GetGuid(); Assert.NotEqual(Guid.Empty, identity);
        Assert.True(draft.Field("/outputs/3/id").ReadOnly);
        draft.AddItem("/outputs"); Assert.NotEqual(identity, draft.Field("/outputs/4/id").Value!.Value.GetGuid());
        Assert.True(draft.Dirty);
    }
    [Fact]
    public void NativeDefaultsAndUnsupportedChoicesComeFromTheHostDescription()
    {
        var draft = new HubConfigurationDraft(Description(), Configuration());
        draft.AddOptional("/sources/0/polling");
        Assert.Equal(30, draft.Field("/sources/0/polling/pollSeconds").Value!.Value.GetDouble());
        Assert.Equal("s", draft.Field("/sources/0/polling/pollSeconds").Schema.GetProperty("x-regain").GetProperty("units").GetString());
        draft.AddItem("/sources"); draft.SelectVariant("/sources/3/backend", "simulated");
        Assert.Equal("switch", draft.Field("/sources/3/backend/deviceType").Value!.Value.GetString());
        Assert.Throws<InvalidOperationException>(() => draft.SelectVariant("/sources/3/backend", "com"));
        var device = draft.Field("/sources/3/backend/deviceType").Schema;
        Assert.Throws<FormatException>(() => draft.ParseScalar(device, "camera"));
        draft.SelectVariant("/sources/3/backend", "alpaca");
        Assert.Equal("externallyManaged", draft.Field("/sources/3/backend/connectionPolicy").Value!.Value.GetString());
        draft.AddOptional("/sources/3/backend/credentialReference", replaceNull: true);
        draft.SetValue("/sources/3/backend/credentialReference", Json("private-reference"));
        Assert.DoesNotContain("private-reference", draft.Preview());
        Assert.Equal("private-reference", draft.Field("/sources/3/backend/credentialReference").Value!.Value.GetString());
    }
    [Fact]
    public void PreviewRedactsNestedReferencesAndPreservesHiddenStoreMetadata()
    {
        var config = JsonNode.Parse(Configuration().GetRawText())!;
        config["identities"] = new JsonObject { ["opaque"] = "hidden-history" };
        config["sources"]![0]!["backend"] = JsonSerializer.SerializeToNode(new { kind = "alpaca", baseUrl = "http://example.test", deviceType = "switch", deviceNumber = 0, credentialReference = "private-reference" });
        var draft = new HubConfigurationDraft(Description(), Json(config));
        draft.SetValue("/outputs/0/label", Json("Edited"));
        var preview = draft.Preview();
        Assert.DoesNotContain("hidden-history", preview); Assert.DoesNotContain("private-reference", preview); Assert.Contains("Edited", preview);
        Assert.Contains("hidden-history", draft.Candidate.GetRawText()); Assert.Contains("private-reference", draft.Candidate.GetRawText());
        Assert.Throws<InvalidOperationException>(() => draft.SetValue("/identities", Json(new { })));
    }
    [Theory]
    [InlineData("", "number")]
    [InlineData("NaN", "number")]
    [InlineData("Infinity", "number")]
    [InlineData("1.5", "integer")]
    [InlineData("4294967296", "integer")]
    public void InvalidScalarInputCannotReplaceAValidDraft(string text, string type)
    {
        var draft = new HubConfigurationDraft(Description(), Configuration());
        var schema = Json(new { type, format = type == "integer" ? "uint32" : "double", minimum = 0.0 });
        Assert.Throws<FormatException>(() => draft.ParseScalar(schema, text)); Assert.False(draft.Dirty);
        Assert.Equal(0, draft.ParseScalar(schema, "0").GetDouble());
    }
    [Fact]
    public void EmptyUnitStringsAreValidAndUnicodeBoundsCountCharacters()
    {
        var draft = new HubConfigurationDraft(Description(), Configuration());
        var units = draft.Field("/outputs/0/device/channels/0/units").Schema;
        Assert.Equal("", draft.ParseScalar(units, "").GetString());
        var label = draft.Field("/outputs/0/label").Schema;
        Assert.Equal(400, draft.ParseScalar(label, string.Concat(Enumerable.Repeat("\U0001F52D", 200))).GetString()!.Length);
        Assert.Throws<FormatException>(() => draft.ParseScalar(label, new string('x', 201)));
        Assert.Throws<FormatException>(() => draft.ParseScalar(label, "\ud800"));
    }
    [Fact]
    public async Task NativeControlsPreserveInvalidTextAcrossCollapseAndBlockStructuralLoss()
    {
        await Sta(() => {
            var draft = new HubConfigurationDraft(Description(), Configuration());
            var failures = new List<string>(); var form = new HubConfigurationForm(draft, () => { }, failures.Add); form.Render();
            var sources = Box(form.Root, "Sources"); sources.IsExpanded = true;
            var source = Box(form.Root, "Simulation switches"); source.IsExpanded = true;
            Box(form.Root, "Polling").IsExpanded = true;
            Click(All<Button>(Box(form.Root, "Polling")).Single(b => (string)b.Content == "Add / customize"));
            var polling = Box(form.Root, "Polling");
            var input = All<TextBox>(form.Root).Single(c => (string?)c.Tag == "/sources/0/polling/pollSeconds");
            input.Text = "NaN"; Assert.Single(form.Errors);
            var before = draft.Candidate.GetProperty("sources").GetArrayLength();
            Click(All<Button>(Box(form.Root, "Sources")).Last(b => (string)b.Content == "Add item"));
            Assert.Equal(before, draft.Candidate.GetProperty("sources").GetArrayLength()); Assert.Contains("highlighted", Assert.Single(failures));
            polling.IsExpanded = false;
            Assert.True(Box(form.Root, "Simulation switches").IsExpanded); Assert.NotNull(Box(form.Root, "Simulation switches").Content);
            polling.IsExpanded = true;
            input = All<TextBox>(form.Root).Single(c => (string?)c.Tag == "/sources/0/polling/pollSeconds");
            Assert.Equal("NaN", input.Text); Assert.Single(form.Errors);
            input.Text = "15"; Assert.Empty(form.Errors); Assert.Equal(15, draft.Field("/sources/0/polling/pollSeconds").Value!.Value.GetDouble());
        });
    }
    [Fact]
    public async Task NativeConnectionPolicyUsesDescribedScalarChoices()
    {
        await Sta(() => {
            var draft = new HubConfigurationDraft(Description(), Configuration());
            draft.SelectVariant("/sources/0/backend", "alpaca");
            var policy = draft.Field("/sources/0/backend/connectionPolicy").Schema;
            Assert.Empty(draft.Description.Variants(policy)); Assert.Empty(draft.Description.Fields(policy));
            var choices = draft.Description.Choices(policy);
            Assert.Equal(new[] { "externallyManaged", "managed" }, choices.Select(c => c.Value));
            Assert.All(choices, c => Assert.NotEmpty(c.Description));
            var form = new HubConfigurationForm(draft, () => { }, message => throw new Exception(message)); form.Render();
            Box(form.Root, "Sources").IsExpanded = true; Box(form.Root, "Simulation switches").IsExpanded = true;
            Box(form.Root, "Backend").IsExpanded = true; Box(form.Root, policy.GetProperty("title").GetString()!).IsExpanded = true;
            var select = All<ComboBox>(form.Root).Single(c => (string?)c.Tag == "/sources/0/backend/connectionPolicy");
            var managed = select.Items.Cast<ComboBoxItem>().Single(c => (string?)c.Tag == "managed");
            Assert.Equal(choices.Single(c => c.Value == "managed").Description, managed.ToolTip);
            select.SelectedItem = managed;
            Assert.Equal("managed", draft.Field("/sources/0/backend/connectionPolicy").Value!.Value.GetString());
        });
    }
    [Fact]
    public async Task NativeCameraChoicesUseSharedCapabilitiesAndActualWpfSelectionEvents()
    {
        await Sta(() => {
            var description = JsonNode.Parse(Description().GetRawText())!;
            description["capabilities"] = new JsonArray("simulation", "cameraSimulation", "proxyOutputs", "cameraOutputs", "focuserOutputs");
            var draft = new HubConfigurationDraft(Json(description),Configuration());
            draft.AddItem("/sources"); draft.SelectVariant("/sources/3/backend","simulated");
            draft.SetValue("/sources/3/backend/deviceType",Json("focuser"));
            draft.SetValue("/sources/3/label",Json("Camera creation source"));
            draft.AddItem("/outputs"); draft.SelectVariant("/outputs/3/device","proxy");
            draft.SetValue("/outputs/3/label",Json("Camera creation output"));
            draft.SetValue("/outputs/3/device/deviceType",Json("focuser"));
            var source = draft.Field("/sources/3/id").Value!.Value.GetGuid();
            draft.SetValue("/outputs/3/device/source",Json(source));
            var output = draft.Field("/outputs/3/id").Value!.Value.GetGuid();
            var changed = 0;
            var form = new HubConfigurationForm(draft,()=>changed++,message=>throw new Exception(message)); form.Render();
            ComboBox Select(string path) {
                // Expand the real lazy WPF tree, including newly rendered descendants.
                for (int depth = 0; depth < 10; depth++)
                    foreach (var expander in All<Expander>(form.Root).ToArray()) expander.IsExpanded = true;
                return All<ComboBox>(form.Root).Single(c=>(string?)c.Tag==path);
            }
            var types = Select("/outputs/3/device/deviceType");
            Assert.Equal(new[]{"camera","focuser"},types.Items.Cast<ComboBoxItem>().Where(c=>c.IsEnabled && c.Tag is string value && value.Length != 0).Select(c=>(string)c.Tag));
            types.SelectedItem = types.Items.Cast<ComboBoxItem>().Single(c=>(string)c.Tag=="camera");
            types = Select("/sources/3/backend/deviceType");
            types.SelectedItem = types.Items.Cast<ComboBoxItem>().Single(c=>(string)c.Tag=="camera");
            Assert.Equal("camera",draft.Field("/outputs/3/device/deviceType").Value!.Value.GetString());
            Assert.Equal("camera",draft.Field("/sources/3/backend/deviceType").Value!.Value.GetString());
            Assert.Equal(source,draft.Field("/outputs/3/device/source").Value!.Value.GetGuid());
            Assert.Equal(output,draft.Field("/outputs/3/id").Value!.Value.GetGuid());
            Assert.Equal(2,changed); Assert.Empty(form.Errors);
        });
    }
    private static Expander Box(DependencyObject root, string header) => All<Expander>(root).Single(e => (string)e.Header == header);
    private static IEnumerable<T> All<T>(DependencyObject root) where T : DependencyObject
    {
        if (root is T match) yield return match;
        foreach (var child in LogicalTreeHelper.GetChildren(root).OfType<DependencyObject>())
            foreach (var item in All<T>(child)) yield return item;
    }
    private static void Click(Button button) => button.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
    private static Task Sta(Action action)
    {
        var completed = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        var thread = new Thread(() => { try { action(); completed.SetResult(true); } catch (Exception error) { completed.SetException(error); } });
        thread.SetApartmentState(ApartmentState.STA); thread.IsBackground = true; thread.Start(); return completed.Task;
    }
}
