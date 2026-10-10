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
        var variants = choices.EnumerateArray().Select(Resolve).ToArray();
        // Scalar enum alternatives carry descriptions too; they are choices,
        // not tagged object variants with a `kind` discriminator.
        if (!variants.All(choice => choice.TryGetProperty("properties", out var properties) && properties.TryGetProperty("kind", out _))) return [];
        return variants.Select(choice => new HubConfigurationChoice(
            choice.Clone(), Text(choice.GetProperty("properties").GetProperty("kind"), "const"),
            Text(choice, "title"), Text(choice, "description"), Available(choice, contextCapabilities))).ToArray();
    }

    public IReadOnlyList<HubConfigurationEnumChoice> Choices(JsonElement node, IEnumerable<string>? contextCapabilities = null)
    {
        node = Resolve(node);
        if (!node.TryGetProperty("enum", out var choices)) {
            if (!node.TryGetProperty("oneOf", out var alternatives)) return [];
            var scalar = alternatives.EnumerateArray().Select(Resolve).ToArray();
            if (!scalar.All(c => c.TryGetProperty("const", out var constant) && constant.ValueKind == JsonValueKind.String)) return [];
            return scalar.Select(c => new HubConfigurationEnumChoice(Text(c, "const"), Available(c, contextCapabilities), Text(c, "description"))).ToArray();
        }
        JsonElement rules = default;
        if (node.TryGetProperty("x-regain", out var metadata)) metadata.TryGetProperty("enumCapabilities", out rules);
        return choices.EnumerateArray().Select(choice => {
            var value = choice.GetString()!;
            var required = Text(rules, value);
            return new HubConfigurationEnumChoice(value, required.Length == 0 || (contextCapabilities ?? capabilities).Contains(required, StringComparer.Ordinal));
        }).ToArray();
    }

    public IReadOnlyList<HubConfigurationField> Fields(JsonElement node, JsonElement? value = null, IEnumerable<string>? contextCapabilities = null, bool isNew = false)
    {
        node = Resolve(node);
        if (node.TryGetProperty("oneOf", out _))
        {
            var kind = value.HasValue ? Text(value.Value, "kind") : "";
            var matches = Variants(node, contextCapabilities).Where(c => c.Kind == kind).ToArray();
            if (matches.Length != 1) return [];
            node = Resolve(matches[0].Schema);
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

public sealed class HubConfigurationEnumChoice(string value, bool enabled, string description = "")
{
    public string Value { get; } = value;
    public bool Enabled { get; } = enabled;
    public string Description { get; } = description;
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
