using System.Text.Json;

namespace Regain.Hub;

public sealed partial class HubEditorSession
{
    public JsonElement? LastDiscovery { get; private set; }
    public JsonElement DiscoveryDescription => Description?.GetProperty("discovery").GetProperty("alpaca")
        ?? throw new InvalidOperationException("Reload the host description first");

    // A catalog read owns no equipment lease and does not invalidate a reviewed
    // draft. Applying a selection remains a separate configuration operation.
    public async Task<JsonElement> DiscoverAlpacaAsync(string baseUrl, string? credentialReference = null,
        CancellationToken cancellation = default)
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
            if (description.GetProperty("opensSource").GetBoolean() || description.GetProperty("writesEquipment").GetBoolean() ||
                description.GetProperty("persistsConfiguration").GetBoolean() ||
                !description.GetProperty("timeoutSeconds").TryGetInt32(out var seconds) || seconds < 1 || seconds > 300)
                throw new HubException(HubFailure.Protocol);
            var revision = Draft!.Revision;
            LastDiscovery = null;
            using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation, lifetime.Token);
            timer.CancelAfter(TimeSpan.FromSeconds(seconds + 5)); started = true;
            var result = await Rpc(new { op = description.GetProperty("operation").GetString(), baseUrl,
                credentialReference, expectedRevision = revision }, timer.Token).ConfigureAwait(false);
            Alive();
            try {
                HubDiagnosticContract.Schema(description.GetProperty("responseSchema"), result);
                if (result.GetProperty("configurationRevision").GetGuid() != revision || Draft.Revision != revision ||
                    !CatalogUrl(result.GetProperty("baseUrl").GetString()!, out var returned) ||
                    requested!.AbsoluteUri.TrimEnd('/') != returned!.AbsoluteUri.TrimEnd('/'))
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
                        !identities.Add(Guid.TryParse(identity, out var id) ? id.ToString() : identity) ||
                        (known ? supported.ValueKind != JsonValueKind.String || supported.GetString() != kind : supported.ValueKind != JsonValueKind.Null)) throw new FormatException();
                }
            } catch { throw new HubException(HubFailure.Protocol); }
            LastDiscovery = result.Clone(); return result.Clone();
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
    private static bool CatalogUrl(string text, out Uri? url) => Uri.TryCreate(text, UriKind.Absolute, out url) &&
        (url.Scheme == Uri.UriSchemeHttp || url.Scheme == Uri.UriSchemeHttps) &&
        url.UserInfo.Length == 0 && url.Query.Length == 0 && url.Fragment.Length == 0;
}
