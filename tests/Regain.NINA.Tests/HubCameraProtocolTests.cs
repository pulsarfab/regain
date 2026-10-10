using System.Text.Json;
using Regain.Hub;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class HubCameraProtocolTests {
    [Fact] public void PropertyKeysMatchGeneratedRustContract() {
        using var contract = JsonDocument.Parse(File.ReadAllText(Path.Combine(AppContext.BaseDirectory,"hub-config.json")));
        var definitions = contract.RootElement.GetProperty("outputDiagnostics").GetProperty("responseSchema").GetProperty("$defs");
        var expected = definitions.GetProperty("CameraProperty").GetProperty("enum").EnumerateArray().Select(value=>value.GetString()).ToArray();
        var actual = Enum.GetValues<HubCameraProperty>().Select(HubCameraProtocol.Key).ToArray();
        Assert.Equal(expected,actual);
        Assert.All(Enum.GetValues<HubCameraProperty>(),property=>Assert.Equal(HubCameraProtocol.Key(property).ToLowerInvariant(),HubCameraProtocol.Member(property)));
    }
    [Theory]
    [InlineData("2026-10-07T01:02:03")]
    [InlineData("2026-10-07T01:02:03.123456789Z")]
    [InlineData("2024-02-29T23:59:60+00:00")]
    public void UpstreamUtcTimesAndFractionalPrecisionArePreserved(string time) {
        Assert.Equal(time,HubCameraProtocol.Validate(HubCameraProperty.LastExposureStartTime,JsonSerializer.SerializeToElement(time)).GetString());
    }
    [Theory]
    [InlineData("2025-02-29T01:02:03")]
    [InlineData("2026-10-07T25:02:03")]
    [InlineData("2026-10-07T01:02:03.")]
    [InlineData("2026-10-07T01:02:03+01:00")]
    public void InvalidTimesAreRejected(string time) => Assert.Throws<HubException>(()=>HubCameraProtocol.Validate(HubCameraProperty.LastExposureStartTime,JsonSerializer.SerializeToElement(time)));
}
