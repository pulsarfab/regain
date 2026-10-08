using System.Net;
using System.Net.Sockets;
using System.Text.Json;

namespace Regain.Hub;

public sealed partial class HubEditorSession
{
    public JsonElement? LastNetworkDiscovery { get; private set; }
    public JsonElement NetworkDiscoveryDescription => Description?.GetProperty("discovery").GetProperty("network")
        ?? throw new InvalidOperationException("Reload the host description first");

    public async Task<JsonElement> SearchAlpacaAsync(CancellationToken cancellation = default)
    {
        using var operation = Borrow();
        await operations.WaitAsync(cancellation).ConfigureAwait(false);
        var started = false;
        try {
            Ready(); var description = NetworkDiscoveryDescription;
            foreach (var key in new[] { "opensSource", "writesEquipment", "persistsConfiguration", "readsCatalog", "usesCredentials" })
                if (description.GetProperty(key).GetBoolean()) throw new HubException(HubFailure.Protocol);
            if (!description.GetProperty("timeoutSeconds").TryGetInt32(out var seconds) || seconds < 1 || seconds > 300)
                throw new HubException(HubFailure.Protocol);
            var revision = Draft!.Revision; LastNetworkDiscovery = null;
            using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation, lifetime.Token);
            timer.CancelAfter(TimeSpan.FromSeconds(seconds + 5)); started = true;
            var result = await Rpc(new { op = description.GetProperty("operation").GetString(), expectedRevision = revision }, timer.Token).ConfigureAwait(false);
            Alive();
            try {
                HubDiagnosticContract.Schema(description.GetProperty("responseSchema"), result);
                if (result.GetProperty("configurationRevision").GetGuid() != revision || Draft.Revision != revision ||
                    result.GetProperty("interfacesFailed").GetUInt32() > result.GetProperty("interfacesTried").GetUInt32() ||
                    result.GetProperty("interfacesFailed").GetUInt32() != 0 && !result.GetProperty("incomplete").GetBoolean()) throw new FormatException();
                var endpoints = new HashSet<string>(StringComparer.Ordinal);
                foreach (var server in result.GetProperty("servers").EnumerateArray()) {
                    var address = server.GetProperty("address").GetString()!;
                    var scope = server.GetProperty("scopeId").GetUInt32(); var port = server.GetProperty("port").GetUInt16();
                    if (address.Contains('%') || !IPAddress.TryParse(address, out var ip) ||
                        (ip.AddressFamily == AddressFamily.InterNetwork ? ip.ToString() != address :
                            address.Any(c => c is not (':' or >= '0' and <= '9' or >= 'a' and <= 'f'))) ||
                        ip.Equals(IPAddress.Any) || ip.Equals(IPAddress.IPv6Any) || ip.Equals(IPAddress.Broadcast) ||
                        ip.IsIPv6Multicast || ip.IsIPv4MappedToIPv6 ||
                        ip.AddressFamily == AddressFamily.InterNetwork && (ip.GetAddressBytes()[0] == 0 || ip.GetAddressBytes()[0] is >= 224 and <= 239) ||
                        !endpoints.Add(ip.ToString() + ":" + scope + ":" + port)) throw new FormatException();
                    var url = server.GetProperty("baseUrl"); var reason = server.GetProperty("unavailableReason");
                    if (ip.IsIPv6LinkLocal) {
                        if (scope == 0 || url.ValueKind != JsonValueKind.Null || reason.GetString() != "scopedIpv6RequiresTransportSupport") throw new FormatException();
                    } else if (scope != 0 || reason.ValueKind != JsonValueKind.Null || url.GetString() !=
                        "http://" + (ip.AddressFamily == AddressFamily.InterNetworkV6 ? "[" + address + "]" : address) + ":" + port)
                        throw new FormatException();
                }
            } catch { throw new HubException(HubFailure.Protocol); }
            LastNetworkDiscovery = result.Clone(); return result.Clone();
        } catch (HubException error) {
            if (started && (error.Failure is not (HubFailure.Remote or HubFailure.Busy or HubFailure.InvalidRequest) ||
                error.Remote?.Code is "revisionConflict" or "disconnected" or "invalidValue")) {
                reviewed = null; State = HubEditorState.Uncertain;
            }
            throw;
        } catch (OperationCanceledException) {
            if (started) { reviewed = null; State = HubEditorState.Uncertain; } throw;
        } finally { operations.Release(); }
    }
}
