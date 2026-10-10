using System.Text.Json;

namespace Regain.Hub;

internal static class HubGroupContract
{
    internal static bool UnknownStart(Exception error) => !(error is HubException hub &&
        (hub.Failure is HubFailure.Busy or HubFailure.InvalidRequest || hub.Failure == HubFailure.Remote &&
         hub.Remote?.Code is not ("uncertain" or "timeout" or "disconnected")));

    internal static Guid PhysicalSource(JsonElement saved, Guid source, string deviceType)
    {
        var seen = new HashSet<Guid>();
        while (seen.Count < 256 && seen.Add(source)) {
            var backend = saved.GetProperty("sources").EnumerateArray().Single(s => s.GetProperty("id").GetGuid() == source).GetProperty("backend");
            if (backend.GetProperty("kind").GetString() != "virtual") return source;
            var output = backend.GetProperty("output").GetGuid();
            var device = saved.GetProperty("outputs").EnumerateArray().Single(o => o.GetProperty("id").GetGuid() == output).GetProperty("device");
            if (device.GetProperty("kind").GetString() != "proxy" || device.GetProperty("deviceType").GetString() != deviceType) break;
            source = device.GetProperty("source").GetGuid();
        }
        throw new HubException(HubFailure.Protocol);
    }

    internal static bool Terminal(JsonElement status) => status.GetProperty("phase").GetString() is not ("connecting" or "running");
    internal static void ImmutableTerminal(JsonElement previous, JsonElement next)
    {
        if (!Terminal(previous)) return;
        // JSON numeric equality uses Double for general telemetry. Sequence
        // identities must also compare UInt64 exactly, including beyond 2^53.
        if (previous.GetProperty("sequence").GetUInt64() != next.GetProperty("sequence").GetUInt64() ||
            !HubDiagnosticContract.Equal(previous, next) || previous.GetProperty("result").ValueKind != JsonValueKind.Null &&
            previous.GetProperty("result").GetProperty("sequence").GetUInt64() != next.GetProperty("result").GetProperty("sequence").GetUInt64())
            throw new HubException(HubFailure.Protocol);
    }
}
