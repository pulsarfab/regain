using System.Text.Json;

namespace Regain.Hub;

public enum HubFocuserProperty { Absolute, MaxStep, MaxIncrement, TempCompAvailable, Position, IsMoving, TempComp, Temperature, StepSize }

/// Typed request/value contract shared by native NINA and COM outputs. Motion
/// admission, source generations and uncertain outcomes remain in the Rust host.
public static class HubFocuserProtocol
{
    public static string Key(HubFocuserProperty property) => property switch {
        HubFocuserProperty.Absolute => "absolute", HubFocuserProperty.MaxStep => "maxStep",
        HubFocuserProperty.MaxIncrement => "maxIncrement", HubFocuserProperty.TempCompAvailable => "tempCompAvailable",
        HubFocuserProperty.Position => "position", HubFocuserProperty.IsMoving => "isMoving",
        HubFocuserProperty.TempComp => "tempComp", HubFocuserProperty.Temperature => "temperature",
        HubFocuserProperty.StepSize => "stepSize", _ => throw new ArgumentOutOfRangeException(nameof(property))
    };
    public static object Read(HubFocuserProperty property) => new { member = "focuser", property = Key(property) };
    public static object Move(int position) => new { member = "moveFocuser", position };
    public static object Halt() => new { member = "haltFocuser" };
    public static object TempComp(bool enabled) => new { member = "focuserTempComp", enabled };
    public static JsonElement Validate(HubFocuserProperty property, JsonElement value)
    {
        bool valid;
        switch (property) {
            case HubFocuserProperty.Absolute: case HubFocuserProperty.TempCompAvailable:
            case HubFocuserProperty.IsMoving: case HubFocuserProperty.TempComp:
                valid = value.ValueKind is JsonValueKind.True or JsonValueKind.False; break;
            case HubFocuserProperty.Position: case HubFocuserProperty.MaxStep: case HubFocuserProperty.MaxIncrement:
                valid = value.ValueKind == JsonValueKind.Number && value.TryGetInt32(out var integer)
                    && integer >= (property == HubFocuserProperty.Position ? 0 : 1); break;
            case HubFocuserProperty.Temperature: case HubFocuserProperty.StepSize:
                valid = value.ValueKind == JsonValueKind.Number && value.TryGetDouble(out var number)
                    && !double.IsNaN(number) && !double.IsInfinity(number)
                    && (property != HubFocuserProperty.StepSize || number > 0); break;
            default: throw new ArgumentOutOfRangeException(nameof(property));
        }
        return valid ? value : throw new HubException(HubFailure.Protocol);
    }
}
