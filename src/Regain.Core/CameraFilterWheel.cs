using System.Text.Json;

namespace Regain.Core;

public sealed record CameraFilterWheelState(int Slots, int Position, bool Moving, bool Verified, int ExpectedPosition, string ParentSerial)
{
    internal static CameraFilterWheelState Parse(JsonElement value)
    {
        int slots = value.GetProperty("slots").GetInt32(), position = value.GetProperty("position").GetInt32();
        int expected = value.GetProperty("expectedPosition").GetInt32();
        if (slots is < 2 or > 16 || position < -1 || position >= slots || expected < 0 || expected >= slots)
            throw new InvalidDataException("Invalid camera filter wheel state");
        return new(slots, position, value.GetProperty("moving").GetBoolean(), value.GetProperty("verified").GetBoolean(), expected,
            value.TryGetProperty("parentSerial", out var serial) ? serial.GetString() ?? "" : "");
    }
}
