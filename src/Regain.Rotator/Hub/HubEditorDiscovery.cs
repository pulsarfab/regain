using System.Text.Json;
using System.Net;

namespace Regain.Hub;

public sealed partial class HubEditorSession
{
    public JsonElement? LastDiscovery { get; private set; }
    private string? discoveryCredentialReference;
    public JsonElement DiscoveryDescription => Description?.GetProperty("discovery").GetProperty("alpaca")
        ?? throw new InvalidOperationException("Reload the host description first");

    // A catalog read owns no equipment lease and does not invalidate a reviewed
    // draft. Applying a selection remains a separate configuration operation.
    public Task<JsonElement> DiscoverAlpacaAsync(string baseUrl, string? credentialReference = null,
        CancellationToken cancellation = default) => DiscoverAlpacaScopedAsync(baseUrl, null, credentialReference, cancellation);

    public async Task<JsonElement> DiscoverAlpacaScopedAsync(string baseUrl, uint? scopeId,
        string? credentialReference = null, CancellationToken cancellation = default)
    {
        using var operation = Borrow();
        await operations.WaitAsync(cancellation).ConfigureAwait(false);
        var started = false;
        try {
            Ready();
            var description = DiscoveryDescription;
            var maximum = description.GetProperty("parameters").GetProperty("baseUrl").GetProperty("maxLength").GetInt32();
            if (baseUrl.Length > maximum || !CatalogUrl(baseUrl, out var requested))
                throw new InvalidOperationException("Enter an HTTP or HTTPS server URL without credentials, query or fragment");
            if (!CatalogScope(requested!, scopeId))
                throw new InvalidOperationException("A literal IPv6 link-local server requires a positive interface scope; leave it empty for other addresses");
            if (description.GetProperty("opensSource").GetBoolean() || description.GetProperty("writesEquipment").GetBoolean() ||
                description.GetProperty("persistsConfiguration").GetBoolean() ||
                !description.GetProperty("timeoutSeconds").TryGetInt32(out var seconds) || seconds < 1 || seconds > 300)
                throw new HubException(HubFailure.Protocol);
            var revision = Draft!.Revision;
            LastDiscovery = null;
            using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation, lifetime.Token);
            timer.CancelAfter(TimeSpan.FromSeconds(seconds + 5)); started = true;
            var command = new Dictionary<string, object?> { ["op"] = description.GetProperty("operation").GetString(),
                ["baseUrl"] = baseUrl, ["credentialReference"] = credentialReference, ["expectedRevision"] = revision };
            if (scopeId.HasValue) command["scopeId"] = scopeId.Value;
            var result = await Rpc(command, timer.Token).ConfigureAwait(false);
            Alive();
            try {
                HubDiagnosticContract.Schema(description.GetProperty("responseSchema"), result);
                if (result.GetProperty("configurationRevision").GetGuid() != revision || Draft.Revision != revision ||
                    !CatalogUrl(result.GetProperty("baseUrl").GetString()!, out var returned) ||
                    requested!.AbsoluteUri.TrimEnd('/') != returned!.AbsoluteUri.TrimEnd('/') ||
                    (result.TryGetProperty("scopeId", out var returnedScope) && returnedScope.ValueKind != JsonValueKind.Null
                        ? returnedScope.GetUInt32() : (uint?)null) != scopeId)
                    throw new FormatException();
                var addresses = new HashSet<string>(StringComparer.Ordinal);
                var identities = new HashSet<string>(StringComparer.Ordinal);
                foreach (var device in result.GetProperty("devices").EnumerateArray()) {
                    var name = device.GetProperty("name").GetString()!;
                    var kind = device.GetProperty("reportedDeviceType").GetString()!.ToLowerInvariant();
                    var identity = device.GetProperty("uniqueId").GetString()!;
                    var supported = device.GetProperty("supportedDeviceType");
                    var known = description.GetProperty("responseSchema").GetProperty("$defs").GetProperty("DeviceType")
                        .GetProperty("enum").EnumerateArray().Any(value => value.GetString() == kind);
                    if (string.IsNullOrWhiteSpace(name) || name.Any(char.IsControl) || string.IsNullOrWhiteSpace(identity) ||
                        !addresses.Add(kind + ":" + device.GetProperty("number").GetUInt32().ToString(System.Globalization.CultureInfo.InvariantCulture)) ||
                        !identities.Add(HubAlpacaIdentity.Normalize(identity)) ||
                        (known ? supported.ValueKind != JsonValueKind.String || supported.GetString() != kind : supported.ValueKind != JsonValueKind.Null)) throw new FormatException();
                }
            } catch { throw new HubException(HubFailure.Protocol); }
            LastDiscovery = result.Clone(); discoveryCredentialReference = credentialReference; return result.Clone();
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
    public Guid AddDiscoveredAlpacaSource(int index)
    {
        Ready();
        if (operations.CurrentCount == 0 || LastDiscovery is not JsonElement catalog)
            throw new InvalidOperationException("Finish a catalog query before selecting a device");
        var id = Draft!.AddDiscoveredAlpacaSource(catalog, index, discoveryCredentialReference);
        Changed(); return id;
    }
    private static bool CatalogScope(Uri url, uint? scope) {
        var local = IPAddress.TryParse(url.Host.Trim('[', ']'), out var ip) && ip.IsIPv6LinkLocal;
        return local ? scope.HasValue && scope.Value > 0 : !scope.HasValue;
    }
    private static bool CatalogUrl(string text, out Uri? url) {
        if (!Uri.TryCreate(text, UriKind.Absolute, out url) ||
            (url.Scheme != Uri.UriSchemeHttp && url.Scheme != Uri.UriSchemeHttps) ||
            url.UserInfo.Length != 0 || text.Contains('?') || text.Contains('#')) return false;
        // Uri can discard a zone suffix. The wire URL must never contain one;
        // scope travels separately and applies to the shared host's interface.
        var start = text.IndexOf("://", StringComparison.Ordinal) + 3;
        var end = text.IndexOf('/', start);
        return !text.Substring(start, (end < 0 ? text.Length : end) - start).Contains('%');
    }
}
