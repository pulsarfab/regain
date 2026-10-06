using System.Text.Json;

namespace Regain.Rotator;

// Reads the Rust description in both net48 ASCOM and net8 NINA setup. Equipment
// keys and policy defaults are deliberately absent from this generic reader.
public sealed class HubConfiguration
{
    private readonly string[] capabilities;
    public JsonElement Root { get; }
    public HubConfiguration(JsonElement description)
    {
        if (description.GetProperty("contractVersion").GetInt32() != 1 ||
            description.GetProperty("schemaVersion").GetInt32() != 1)
            throw new InvalidOperationException("Unsupported hub configuration contract");
        Root = description.GetProperty("schema").Clone();
        capabilities = description.TryGetProperty("capabilities", out var values)
            ? values.EnumerateArray().Select(v => v.GetString()!).ToArray() : [];
    }

    public JsonElement Resolve(JsonElement node)
    {
        var seen = new HashSet<string>(StringComparer.Ordinal);
        while (node.TryGetProperty("$ref", out var reference))
        {
            var key = reference.GetString()!;
            const string prefix = "#/$defs/";
            if (!key.StartsWith(prefix, StringComparison.Ordinal) || !seen.Add(key))
                throw new InvalidOperationException("Invalid configuration schema reference");
            var name = key.Substring(prefix.Length).Replace("~1", "/").Replace("~0", "~");
            if (!Root.GetProperty("$defs").TryGetProperty(name, out var target))
                throw new InvalidOperationException("Unknown configuration schema reference");
            var merged = target.EnumerateObject().ToDictionary(p => p.Name, p => p.Value.Clone(), StringComparer.Ordinal);
            foreach (var property in node.EnumerateObject())
                if (property.Name != "$ref") merged[property.Name] = property.Value.Clone();
            node = JsonSerializer.SerializeToElement(merged);
        }
        return node;
    }

    public bool Available(JsonElement node, IEnumerable<string>? contextCapabilities = null)
    {
        node = Resolve(node);
        return !node.TryGetProperty("x-regain", out var metadata) ||
            !metadata.TryGetProperty("requiresCapability", out var required) ||
            (contextCapabilities ?? capabilities).Contains(required.GetString(), StringComparer.Ordinal);
    }

    public IReadOnlyList<HubConfigurationChoice> Variants(JsonElement node, IEnumerable<string>? contextCapabilities = null)
    {
        node = Resolve(node);
        if (!node.TryGetProperty("oneOf", out var choices)) return [];
        return choices.EnumerateArray().Select(choice => new HubConfigurationChoice(
            choice.Clone(), Text(choice.GetProperty("properties").GetProperty("kind"), "const"),
            Text(choice, "title"), Text(choice, "description"), Available(choice, contextCapabilities))).ToArray();
    }

    public IReadOnlyList<HubConfigurationField> Fields(JsonElement node, JsonElement? value = null, IEnumerable<string>? contextCapabilities = null, bool isNew = false)
    {
        node = Resolve(node);
        if (node.TryGetProperty("oneOf", out var choices))
        {
            var kind = value.HasValue ? Text(value.Value, "kind") : "";
            var matches = choices.EnumerateArray().Where(c => Text(c.GetProperty("properties").GetProperty("kind"), "const") == kind).ToArray();
            if (matches.Length != 1) return [];
            node = Resolve(matches[0]);
        }
        if (!node.TryGetProperty("properties", out var properties)) return [];
        var required = node.TryGetProperty("required", out var requiredNames)
            ? new HashSet<string>(requiredNames.EnumerateArray().Select(n => n.GetString()!), StringComparer.Ordinal) : [];
        var fields = new List<HubConfigurationField>();
        foreach (var property in properties.EnumerateObject())
        {
            var field = Resolve(property.Value);
            if (field.TryGetProperty("x-regain", out var metadata) && Flag(metadata, "hidden")) continue;
            JsonElement? current = null;
            if (value.HasValue && value.Value.TryGetProperty(property.Name, out var specified)) current = specified.Clone();
            else if (field.TryGetProperty("default", out var fallback)) current = fallback.Clone();
            fields.Add(new HubConfigurationField(property.Name, field.Clone(), Text(field, "title"),
                Text(field, "description"), required.Contains(property.Name), Available(field, contextCapabilities),
                Flag(field, "readOnly") || (field.TryGetProperty("x-regain", out var rules) && Flag(rules, "immutableAfterCreate") && !isNew), current));
        }
        return fields;
    }
    private static bool Flag(JsonElement value, string key) => value.TryGetProperty(key, out var flag) && flag.ValueKind == JsonValueKind.True;
    private static string Text(JsonElement value, string key) => value.ValueKind == JsonValueKind.Object && value.TryGetProperty(key, out var text) ? text.GetString() ?? "" : "";
}

public sealed class HubConfigurationChoice(JsonElement schema, string kind, string title, string description, bool enabled)
{
    public JsonElement Schema { get; } = schema;
    public string Kind { get; } = kind;
    public string Title { get; } = title;
    public string Description { get; } = description;
    public bool Enabled { get; } = enabled;
}
public sealed class HubConfigurationField(string key, JsonElement schema, string title, string description, bool required, bool enabled, bool readOnly, JsonElement? value)
{
    public string Key { get; } = key;
    public JsonElement Schema { get; } = schema;
    public string Title { get; } = title;
    public string Description { get; } = description;
    public bool Required { get; } = required;
    public bool Enabled { get; } = enabled;
    public bool ReadOnly { get; } = readOnly;
    public JsonElement? Value { get; } = value;
}
