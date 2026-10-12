using System.Text.Json;

namespace Regain.Core;

/// <summary>Live camera USB lifecycle anchors emitted by its owning supervisor.</summary>
public enum CameraUsbRecoveryPhase { Resetting, CameraReturned }
public sealed record CameraUsbRecoveryEvent(CameraUsbRecoveryPhase Phase, ulong? RecoveryId, long? TimestampUnixUs = null);

internal static class CameraUsbRecovery
{
    private const string Prefix = "REGAIN_DIAGNOSTIC ";

    internal static bool TryParse(string line, int processId, bool supervised, out CameraUsbRecoveryEvent recovery)
    {
        recovery = null!;
        if (!supervised || !line.StartsWith(Prefix, StringComparison.Ordinal)) return false;
        try
        {
            using var document = JsonDocument.Parse(line.AsMemory(Prefix.Length));
            var record = document.RootElement;
            if (record.ValueKind != JsonValueKind.Object || ContainsReplay(record) || record.TryGetProperty("managedContext", out _)
                || !Number(record, "version", out int version) || version != 1
                || !Number(record, "pid", out int origin) || origin != processId
                || !record.TryGetProperty("supervisor", out var owner) || owner.ValueKind != JsonValueKind.Object
                || !Number(owner, "pid", out int supervisor) || supervisor != processId
                || !record.TryGetProperty("message", out var message) || message.ValueKind != JsonValueKind.String
                || !record.TryGetProperty("event", out var kind) || kind.ValueKind != JsonValueKind.String) return false;
            CameraUsbRecoveryPhase phase;
            switch (kind.GetString())
            {
                case "usb.reset": phase = CameraUsbRecoveryPhase.Resetting; break;
                case "usb.returned": phase = CameraUsbRecoveryPhase.CameraReturned; break;
                default: return false;
            }
            ulong? id = null;
            if (record.TryGetProperty("details", out var details))
            {
                if (details.ValueKind != JsonValueKind.Object) return false;
                if (details.TryGetProperty("usbPeripheralRecoveryId", out var value) && value.ValueKind != JsonValueKind.Null)
                {
                    if (value.ValueKind != JsonValueKind.Number || !value.TryGetUInt64(out var found) || found == 0) return false;
                    id = found;
                }
            }
            long? timestamp = null;
            if (record.TryGetProperty("timestampUnixUs", out var time) && time.ValueKind != JsonValueKind.Null)
            {
                if (time.ValueKind != JsonValueKind.Number || !time.TryGetInt64(out var found) || found <= 0) return false;
                timestamp = found;
            }
            recovery = new(phase, id, timestamp);
            return true;
        }
        catch (JsonException) { return false; }
    }

    private static bool Number(JsonElement record, string name, out int value)
    {
        value = default;
        return record.TryGetProperty(name, out var property) && property.ValueKind == JsonValueKind.Number && property.TryGetInt32(out value);
    }

    private static bool ContainsReplay(JsonElement value)
    {
        if (value.ValueKind == JsonValueKind.Object)
            foreach (var property in value.EnumerateObject())
            {
                if (property.Name is "flightRecorderBatch" or "flightRecorderAfterFault" or "replay" or "replayed"
                    || ContainsReplay(property.Value)) return true;
            }
        else if (value.ValueKind == JsonValueKind.Array)
            foreach (var item in value.EnumerateArray())
                if (ContainsReplay(item)) return true;
        return false;
    }
}
