using System.Text.Json;

namespace Regain.Hub;

public enum HubRotatorProperty { CanReverse, IsMoving, MechanicalPosition, Position, Reverse, StepSize, TargetPosition }

/// Frontends share wire names and strict Single-range readings. Coordinates,
/// command ownership and persistent references remain in the Rust source.
public static class HubRotatorProtocol
{
    public static string Key(HubRotatorProperty property) => property switch {
        HubRotatorProperty.CanReverse => "canReverse", HubRotatorProperty.IsMoving => "isMoving",
        HubRotatorProperty.MechanicalPosition => "mechanicalPosition", HubRotatorProperty.Position => "position",
        HubRotatorProperty.Reverse => "reverse", HubRotatorProperty.StepSize => "stepSize",
        HubRotatorProperty.TargetPosition => "targetPosition", _ => throw new ArgumentOutOfRangeException(nameof(property))
    };
    public static object Read(HubRotatorProperty property) => new { member = "rotator", property = Key(property) };
    public static object Move(float degrees) => new { member = "moveRotator", degrees };
    public static object MoveTracked(float degrees) => new { member = "moveRotatorTracked", degrees };
    public static object MoveAbsolute(float degrees) => new { member = "moveAbsoluteRotator", degrees };
    public static object MoveMechanical(float degrees) => new { member = "moveMechanicalRotator", degrees };
    public static object Sync(float degrees) => new { member = "syncRotator", degrees };
    public static object Halt() => new { member = "haltRotator" };
    public static object Reverse(bool enabled) => new { member = "rotatorReverse", enabled };
    public static JsonElement Validate(HubRotatorProperty property, JsonElement value)
    {
        var boolean = property is HubRotatorProperty.CanReverse or HubRotatorProperty.IsMoving or HubRotatorProperty.Reverse;
        var valid = boolean ? value.ValueKind is JsonValueKind.True or JsonValueKind.False
            : value.ValueKind == JsonValueKind.Number && value.TryGetDouble(out var number)
                && !double.IsNaN(number) && !double.IsInfinity(number) && number <= float.MaxValue
                && (property == HubRotatorProperty.StepSize ? number > 0 && (float)number > 0
                    : number >= 0 && number < 360 && (float)number < 360);
        return valid ? value : throw new HubException(HubFailure.Protocol);
    }
    public static (float Expected, float Accepted) TargetReceipt(JsonElement receipt)
    {
        HubWire.Members(receipt, ["expectedTarget", "targetPosition"]);
        return (Validate(HubRotatorProperty.TargetPosition, receipt.GetProperty("expectedTarget")).GetSingle(),
            Validate(HubRotatorProperty.TargetPosition, receipt.GetProperty("targetPosition")).GetSingle());
    }
    public static void ValidateCommand(float degrees, bool absolute)
    {
        if (float.IsNaN(degrees) || float.IsInfinity(degrees) || absolute && (degrees < 0 || degrees >= 360))
            throw new ArgumentOutOfRangeException(nameof(degrees));
    }
    public static float AngularError(float actual, float expected)
    {
        var difference = Math.Abs(actual - expected) % 360;
        return Math.Min(difference, 360 - difference);
    }
}
