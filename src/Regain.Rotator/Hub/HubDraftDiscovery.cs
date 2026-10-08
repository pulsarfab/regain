using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace Regain.Hub;

public sealed partial class HubConfigurationDraft
{
    internal Guid AddDiscoveredAlpacaSource(JsonElement catalog, int index, string? credentialReference)
    {
        if (catalog.GetProperty("configurationRevision").GetGuid() != Revision)
            throw new InvalidOperationException("Reload and query the catalog again before adding a source");
        var devices = catalog.GetProperty("devices");
        if (index < 0 || index >= devices.GetArrayLength()) throw new InvalidOperationException("Select a catalog entry");
        var device = devices[index];
        if (device.GetProperty("supportedDeviceType").ValueKind != JsonValueKind.String)
            throw new InvalidOperationException("This device class is not supported by Regain Hub");
        var type = device.GetProperty("supportedDeviceType").GetString()!;
        var number = device.GetProperty("number").GetUInt32(); var identity = device.GetProperty("uniqueId").GetString()!;
        var server = catalog.GetProperty("baseUrl").GetString()!;
        var scope = catalog.TryGetProperty("scopeId", out var scoped) && scoped.ValueKind != JsonValueKind.Null ? scoped.GetUInt32() : (uint?)null;
        var sources = value["sources"]!.AsArray(); var arraySchema = Field("/sources").Schema;
        if (sources.Count >= arraySchema.GetProperty("maxItems").GetInt32()) throw new InvalidOperationException("Configuration source limit reached");
        foreach (var existing in sources) {
            var backend = existing!["backend"]!.AsObject();
            if (backend["kind"]!.GetValue<string>() != "alpaca") continue;
            if (backend["uniqueId"] is JsonNode pin && HubAlpacaIdentity.Normalize(pin.GetValue<string>()) == HubAlpacaIdentity.Normalize(identity) ||
                new Uri(backend["baseUrl"]!.GetValue<string>()).AbsoluteUri.TrimEnd('/') == new Uri(server).AbsoluteUri.TrimEnd('/') &&
                (backend["scopeId"] is JsonNode existingScope ? existingScope.GetValue<uint>() : (uint?)null) == scope &&
                backend["deviceType"]!.GetValue<string>() == type && backend["deviceNumber"]!.GetValue<uint>() == number)
                throw new InvalidOperationException("This Alpaca device already has a source. Share its existing source ID");
        }
        var schema = Description.Resolve(arraySchema.GetProperty("items"));
        var choice = Description.Variants(schema.GetProperty("properties").GetProperty("backend"))
            .SingleOrDefault(variant => variant.Kind == "alpaca" && variant.Enabled)
            ?? throw new InvalidOperationException("Alpaca sources are unavailable in this host");
        // Construct off-draft first. Rejection cannot leave an incomplete source
        // or revoke an already reviewed candidate.
        var source = JsonNode.Parse(InitialValue(schema).GetRawText())!.AsObject();
        var prepared = JsonNode.Parse(InitialValue(choice.Schema).GetRawText())!.AsObject();
        prepared["baseUrl"] = server; prepared["deviceType"] = type; prepared["deviceNumber"] = number;
        prepared["uniqueId"] = identity;
        if (scope.HasValue) prepared["scopeId"] = scope.Value;
        if (credentialReference is not null) prepared["credentialReference"] = credentialReference;
        source["backend"] = prepared;
        var maximum = schema.GetProperty("properties").GetProperty("label").GetProperty("maxLength").GetInt32();
        source["label"] = CatalogLabel(device.GetProperty("name").GetString()!, maximum);
        var id = Guid.Parse(source["id"]!.GetValue<string>());
        sources.Add(source); Version++; return id;
    }
    private static string CatalogLabel(string text, int maximum)
    {
        var result = new StringBuilder();
        for (int i = 0, count = 0; i < text.Length && count < maximum; i++, count++) {
            result.Append(text[i]); if (char.IsHighSurrogate(text[i])) result.Append(text[++i]);
        }
        return result.ToString();
    }
}
