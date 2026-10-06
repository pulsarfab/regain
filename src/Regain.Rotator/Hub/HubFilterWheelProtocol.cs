using System.Text;
using System.Text.Json;

namespace Regain.Hub;

public enum HubFilterWheelProperty { Names, FocusOffsets, Position }

/// Shared native wire bounds. Live slot pairing and command ownership belong
/// to Rust; neither frontend invents metadata or applies focuser offsets.
public static class HubFilterWheelProtocol
{
    public const int MaximumSlots = 1024;
    private const int MaximumTextBytes = 1024 * 1024;
    public static string Key(HubFilterWheelProperty property) => property switch {
        HubFilterWheelProperty.Names => "names", HubFilterWheelProperty.FocusOffsets => "focusOffsets",
        HubFilterWheelProperty.Position => "position", _ => throw new ArgumentOutOfRangeException(nameof(property))
    };
    public static object Read(HubFilterWheelProperty property) => new { member = "filterWheel", property = Key(property) };
    public static object Move(int position)
    {
        if (position < 0 || position >= MaximumSlots) throw new ArgumentOutOfRangeException(nameof(position));
        return new { member = "moveFilterWheel", position };
    }
    public static JsonElement Validate(HubFilterWheelProperty property, JsonElement value)
    {
        try {
            if (property == HubFilterWheelProperty.Position) {
                if (value.ValueKind == JsonValueKind.Number && value.TryGetInt32(out var position) && position >= -1 && position < MaximumSlots) return value;
            } else if (value.ValueKind == JsonValueKind.Array && value.GetArrayLength() is > 0 and <= MaximumSlots) {
                if (property == HubFilterWheelProperty.Names) {
                    long bytes = 0;
                    foreach (var item in value.EnumerateArray()) {
                        if (item.ValueKind != JsonValueKind.String) throw new HubException(HubFailure.Protocol);
                        bytes += Encoding.UTF8.GetByteCount(item.GetString()!);
                    }
                    if (bytes <= MaximumTextBytes) return value;
                } else if (property == HubFilterWheelProperty.FocusOffsets) {
                    bool reference = false;
                    foreach (var item in value.EnumerateArray()) {
                        if (item.ValueKind != JsonValueKind.Number || !item.TryGetInt32(out var offset)) throw new HubException(HubFailure.Protocol);
                        reference |= offset == 0;
                    }
                    if (reference) return value;
                }
            }
        } catch (Exception error) when (error is InvalidOperationException or JsonException or ArgumentException) {
            throw new HubException(HubFailure.Protocol);
        }
        throw new HubException(HubFailure.Protocol);
    }
    public static (string[] Names, int[] FocusOffsets) Metadata(JsonElement names, JsonElement offsets)
    {
        Validate(HubFilterWheelProperty.Names,names); Validate(HubFilterWheelProperty.FocusOffsets,offsets);
        if (names.GetArrayLength() != offsets.GetArrayLength()) throw new HubException(HubFailure.Protocol);
        return (names.EnumerateArray().Select(item => item.GetString()!).ToArray(),offsets.EnumerateArray().Select(item => item.GetInt32()).ToArray());
    }
}
