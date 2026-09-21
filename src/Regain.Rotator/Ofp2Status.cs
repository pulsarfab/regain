using System.Text.Json;
using System.Text.Json.Serialization;

namespace Regain.Rotator;

public sealed class Ofp2Status
{
    [JsonPropertyName("cover")] public string Cover { get; set; } = "unknown";
    [JsonPropertyName("calibrator_on")] public bool CalibratorOn { get; set; }
    [JsonPropertyName("brightness")] public int Brightness { get; set; }
    [JsonPropertyName("max_brightness")] public int MaxBrightness { get; set; }
    public static Ofp2Status Read(AccessorySession session) =>
        JsonSerializer.Deserialize<Ofp2Status>(session.Request(new { command = "status" }).GetRawText())!;
}
