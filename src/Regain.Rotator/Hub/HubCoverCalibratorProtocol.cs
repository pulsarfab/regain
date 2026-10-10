using System.Text.Json;

namespace Regain.Hub;

public enum HubCoverCalibratorProperty { Brightness, MaxBrightness, CoverState, CalibratorState, CoverMoving, CalibratorChanging }

/// Shared native wire validation. Source capabilities, completion and command
/// ownership remain in Rust, including legacy V1 completion conversion.
public static class HubCoverCalibratorProtocol
{
    public static string Key(HubCoverCalibratorProperty property) => property switch {
        HubCoverCalibratorProperty.Brightness => "brightness", HubCoverCalibratorProperty.MaxBrightness => "maxBrightness",
        HubCoverCalibratorProperty.CoverState => "coverState", HubCoverCalibratorProperty.CalibratorState => "calibratorState",
        HubCoverCalibratorProperty.CoverMoving => "coverMoving", HubCoverCalibratorProperty.CalibratorChanging => "calibratorChanging",
        _ => throw new ArgumentOutOfRangeException(nameof(property))
    };
    public static object Read(HubCoverCalibratorProperty property) => new { member = "coverCalibrator", property = Key(property) };
    public static object On(int brightness)
    {
        if (brightness < 0) throw new ArgumentOutOfRangeException(nameof(brightness));
        return new { member = "calibratorOn", brightness };
    }
    public static object Off() => new { member = "calibratorOff" };
    public static object Open() => new { member = "openCover" };
    public static object Close() => new { member = "closeCover" };
    public static object Halt() => new { member = "haltCover" };
    public static JsonElement Validate(HubCoverCalibratorProperty property, JsonElement value)
    {
        if (property is HubCoverCalibratorProperty.CoverMoving or HubCoverCalibratorProperty.CalibratorChanging) {
            if (value.ValueKind is JsonValueKind.True or JsonValueKind.False) return value;
        } else if (value.ValueKind == JsonValueKind.Number && value.TryGetInt32(out var number) && number >= 0) {
            if (property is HubCoverCalibratorProperty.CoverState or HubCoverCalibratorProperty.CalibratorState) {
                if (number <= 5) return value;
            } else if (property == HubCoverCalibratorProperty.Brightness || property == HubCoverCalibratorProperty.MaxBrightness && number > 0) return value;
        }
        throw new HubException(HubFailure.Protocol);
    }
    public static HubCoverCalibratorProperty StateProperty(string? name) => name switch {
        "Brightness" => HubCoverCalibratorProperty.Brightness, "CalibratorState" => HubCoverCalibratorProperty.CalibratorState,
        "CoverState" => HubCoverCalibratorProperty.CoverState, "CoverMoving" => HubCoverCalibratorProperty.CoverMoving,
        "CalibratorChanging" => HubCoverCalibratorProperty.CalibratorChanging,
        _ => throw new HubException(HubFailure.Protocol)
    };
}
