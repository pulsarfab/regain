using System.Text.Json;
using Regain.Rotator;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class HubConfigurationTests
{
    private static JsonElement Contract()
    {
        using var doc=JsonDocument.Parse(File.ReadAllText(Path.Combine(AppContext.BaseDirectory,"hub-config.json")));
        return doc.RootElement.Clone();
    }
    [Fact]
    public void NativeSetupReadsTheSameChoicesDefaultsAndCapabilitiesAsTheWeb()
    {
        var reader=new HubConfiguration(Contract());
        var source=reader.Root.GetProperty("$defs").GetProperty("SourceBackend");
        var choices=reader.Variants(source,["alpacaSources"]);
        Assert.True(choices.Single(c=>c.Kind=="alpaca").Enabled);
        Assert.False(choices.Single(c=>c.Kind=="com").Enabled);
        var value=JsonSerializer.SerializeToElement(new {kind="alpaca",baseUrl="http://localhost:11111",deviceNumber=0});
        var fields=reader.Fields(source,value);
        Assert.DoesNotContain(fields,f=>f.Key=="progId");
        Assert.Equal("http://localhost:11111",fields.Single(f=>f.Key=="baseUrl").Value!.Value.GetString());
        Assert.Equal("externallyManaged",fields.Single(f=>f.Key=="connectionPolicy").Value!.Value.GetString());
        Assert.True(reader.Fields(reader.Root).Single(f=>f.Key=="revision").ReadOnly);
        Assert.DoesNotContain(reader.Fields(reader.Root),f=>f.Key=="identities");
        var output=reader.Root.GetProperty("$defs").GetProperty("OutputConfig");
        Assert.True(reader.Fields(output).Single(f=>f.Key=="number").ReadOnly);
        Assert.False(reader.Fields(output,isNew:true).Single(f=>f.Key=="number").ReadOnly);
        var policy=reader.Fields(reader.Root.GetProperty("$defs").GetProperty("SafetyPolicy"));
        var age=policy.Single(f=>f.Key=="maximumSafeAgeSeconds");
        Assert.Equal(90,age.Value!.Value.GetDouble());
        Assert.Equal("s",age.Schema.GetProperty("x-regain").GetProperty("units").GetString());
        Assert.Equal("integer",policy.Single(f=>f.Key=="safeReadingsToSafe").Schema.GetProperty("type").GetString());
        Assert.All(policy,f=>Assert.False(string.IsNullOrWhiteSpace(f.Description)));
        var nativeDevice=choices.Single(c=>c.Kind=="native").Schema.GetProperty("properties").GetProperty("device");
        Assert.False(reader.Choices(nativeDevice).Single(c=>c.Value=="camera-direct").Enabled);
        Assert.True(reader.Choices(nativeDevice).Single(c=>c.Value=="efw").Enabled);
        Assert.True(reader.Choices(nativeDevice,["nativeCameraSources"]).Single(c=>c.Value=="camera-direct").Enabled);
        var membership=reader.Root.GetProperty("$defs").GetProperty("SafetyMember");
        Assert.Equal("source",reader.Fields(membership).Single(f=>f.Key=="source").Schema.GetProperty("x-regain").GetProperty("reference").GetString());
    }
    [Fact]
    public void ComClassAndBitnessChoicesRespectTheInstalledHostCapabilities()
    {
        var reader=new HubConfiguration(Contract());
        var source=reader.Root.GetProperty("$defs").GetProperty("SourceBackend");
        var com=reader.Variants(source,["comSources"]).Single(v=>v.Kind=="com");
        Assert.True(com.Enabled);
        var properties=com.Schema.GetProperty("properties");
        Assert.Equal(new[]{"switch","safetymonitor","observingconditions","focuser","rotator"},reader.Choices(properties.GetProperty("deviceType")).Where(c=>c.Enabled).Select(c=>c.Value));
        var bitness=reader.Choices(properties.GetProperty("bitness"),["comX86Sources"]);
        Assert.True(bitness.Single(c=>c.Value=="x86").Enabled);
        Assert.False(bitness.Single(c=>c.Value=="x64").Enabled);
    }
    [Fact]
    public void TypedProxyChoicesExposeOnlyPublishedClasses()
    {
        var reader = new HubConfiguration(Contract());
        var device = reader.Root.GetProperty("$defs").GetProperty("VirtualDevice");
        Assert.False(reader.Variants(device).Single(v => v.Kind == "proxy").Enabled);
        var proxy = reader.Variants(device, ["proxyOutputs", "focuserOutputs"]).Single(v => v.Kind == "proxy");
        Assert.True(proxy.Enabled);
        var classes = proxy.Schema.GetProperty("properties").GetProperty("deviceType");
        Assert.Equal(new[] { "focuser" }, reader.Choices(classes, ["focuserOutputs"]).Where(c => c.Enabled).Select(c => c.Value));
        Assert.Equal(new[] { "rotator" }, reader.Choices(classes, ["rotatorOutputs"]).Where(c => c.Enabled).Select(c => c.Value));
        Assert.Equal(new[] { "focuser", "rotator" }, reader.Choices(classes, ["focuserOutputs", "rotatorOutputs"]).Where(c => c.Enabled).Select(c => c.Value));
        Assert.All(reader.Choices(classes), c => Assert.False(c.Enabled));
    }
    [Fact]
    public void UnknownContractsAndReferencesAreRejected()
    {
        Assert.Throws<InvalidOperationException>(()=>new HubConfiguration(JsonSerializer.SerializeToElement(new {contractVersion=2,schemaVersion=1})));
        var reader=new HubConfiguration(Contract());
        using var unknown=JsonDocument.Parse("{\"$ref\":\"#/$defs/missing\"}");
        Assert.Throws<InvalidOperationException>(()=>reader.Resolve(unknown.RootElement));
    }
}
