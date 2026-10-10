using System.Globalization;
using System.Text.Json;
using System.Text.Json.Nodes;
using Regain.Rotator;

namespace Regain.Hub;

/// Schema-driven draft utilities shared by the native frontends. The host,
/// including its identity ledger and cross-field checks, authorizes Apply.
public sealed partial class HubConfigurationDraft
{
    private readonly JsonObject baseline;
    private readonly JsonObject value;
    public HubConfiguration Description { get; }
    public long Version { get; private set; }
    public Guid Revision { get; }
    public JsonElement Candidate => Element(value);
    public bool Dirty => value.ToJsonString() != baseline.ToJsonString();
    public HubConfigurationDraft(JsonElement description, JsonElement configuration)
    {
        Description = new(description);
        baseline = JsonNode.Parse(configuration.GetRawText())?.AsObject() ?? throw new InvalidOperationException("Missing hub configuration");
        value = (JsonObject)baseline.DeepClone();
        Revision = configuration.GetProperty("revision").GetGuid();
        if (Revision == Guid.Empty) throw new InvalidOperationException("Invalid hub configuration revision");
    }
    public HubDraftField Field(string path)
    {
        JsonNode? current = value, original = baseline;
        var schema = Description.Root; bool required = true, exists = true, prior = true, inherited = false, enabled = true;
        foreach (var key in Parts(path)) {
            inherited |= ReadOnly(schema, prior) || Hidden(schema);
            schema = Active(schema, current);
            enabled &= Description.Available(schema);
            if (current is JsonArray array) {
                var index = Index(key, array.Count);
                schema = Description.Resolve(schema.GetProperty("items")); current = array[index]; exists = true;
                var old = original as JsonArray;
                var identity = current is JsonObject item ? item["id"]?.GetValue<string>() : null;
                original = identity is not null ? old?.FirstOrDefault(n => n is JsonObject record && record["id"]?.GetValue<string>() == identity)
                    : old is not null && index < old.Count ? old[index] : null;
                prior = original is not null; required = true;
            } else {
                if (current is not JsonObject record || !schema.TryGetProperty("properties", out var properties) || !properties.TryGetProperty(key, out var child))
                    throw new InvalidOperationException("Unknown configuration field");
                required = schema.TryGetProperty("required", out var names) && names.EnumerateArray().Any(n => n.GetString() == key);
                exists = record.TryGetPropertyValue(key, out current);
                prior = original is JsonObject old && old.ContainsKey(key);
                original = prior ? ((JsonObject)original!)[key] : null;
                schema = Description.Resolve(child);
            }
        }
        return new(path, schema.Clone(), exists ? Element(current) : null, prior ? Element(original) : null, required,
            inherited || ReadOnly(schema, prior) || Hidden(schema), enabled && Description.Available(schema));
    }
    public void SetValue(string path, JsonElement replacement)
    {
        var field = Editable(path);
        if (field.Schema.TryGetProperty("const", out _)) throw new InvalidOperationException("A constant cannot be edited");
        if (Type(field.Schema) is "object" or "array" || Description.Variants(field.Schema).Count != 0 ||
            replacement.ValueKind is JsonValueKind.Object or JsonValueKind.Array)
            throw new InvalidOperationException("Use the structural editor to change collections or configuration types");
        Assign(path, replacement);
    }
    private void Assign(string path, JsonElement replacement)
    {
        var (parent, key) = Parent(path);
        var next = JsonNode.Parse(replacement.GetRawText());
        if (parent is JsonArray array) array[Index(key, array.Count)] = next; else ((JsonObject)parent)[key] = next;
        Version++;
    }
    public void AddOptional(string path, bool replaceNull = false)
    {
        var field = Editable(path);
        if ((field.Required && !replaceNull) || (field.Value.HasValue && !(replaceNull && field.Value.Value.ValueKind == JsonValueKind.Null)))
            throw new InvalidOperationException("This optional field already exists or is required");
        Assign(path, InitialValue(field.Schema, ignoreDefault: replaceNull));
    }
    public void RemoveOptional(string path)
    {
        var field = Editable(path);
        if (field.Required) throw new InvalidOperationException("A required field cannot be removed");
        var (parent, key) = Parent(path); ((JsonObject)parent).Remove(key); Version++;
    }
    public void AddItem(string path)
    {
        var field = Editable(path); var array = Node(path) as JsonArray ?? throw new InvalidOperationException("Expected an array");
        if (field.Schema.TryGetProperty("maxItems", out var maximum) && array.Count >= maximum.GetInt32())
            throw new InvalidOperationException("Configuration collection limit reached");
        array.Add(JsonNode.Parse(InitialValue(field.Schema.GetProperty("items")).GetRawText())); Version++;
    }
    public void RemoveItem(string path, int index)
    {
        Editable(path); var array = Node(path) as JsonArray ?? throw new InvalidOperationException("Expected an array");
        array.RemoveAt(Index(index.ToString(CultureInfo.InvariantCulture), array.Count)); Version++;
    }
    public void SelectVariant(string path, string kind)
    {
        var field = Editable(path);
        var choice = Description.Variants(field.Schema).SingleOrDefault(c => c.Kind == kind && c.Enabled)
            ?? throw new InvalidOperationException("Configuration choice is not available");
        Assign(path, InitialValue(choice.Schema));
    }
    public JsonElement InitialValue(JsonElement raw, bool ignoreDefault = false) => Element(Initial(raw, ignoreDefault, 0));
    private JsonNode? Initial(JsonElement raw, bool ignoreDefault, int depth)
    {
        if (depth > 32) throw new InvalidOperationException("Configuration schema is too deep");
        var schema = Description.Resolve(raw);
        if (!ignoreDefault && schema.TryGetProperty("default", out var fallback)) return JsonNode.Parse(fallback.GetRawText());
        if (schema.TryGetProperty("const", out var constant)) return JsonNode.Parse(constant.GetRawText());
        if (schema.TryGetProperty("oneOf", out _)) {
            var scalar = Description.Choices(schema);
            if (scalar.Count != 0) return JsonValue.Create(scalar.FirstOrDefault(c => c.Enabled)?.Value ?? throw new InvalidOperationException("No available configuration choice"));
            var choice = Description.Variants(schema).FirstOrDefault(c => c.Enabled) ?? throw new InvalidOperationException("No available configuration choice");
            return Initial(choice.Schema, false, depth + 1);
        }
        if (schema.TryGetProperty("anyOf", out var nullable)) return Initial(nullable.EnumerateArray().First(n => Type(n) != "null"), false, depth + 1);
        if (Text(schema, "format") == "uuid") return JsonValue.Create(ReadOnly(schema, false) ? Guid.NewGuid().ToString("D") : "");
        if (schema.TryGetProperty("enum", out _)) return JsonValue.Create(Description.Choices(schema).FirstOrDefault(c => c.Enabled)?.Value ?? "");
        switch (Type(schema)) {
            case "array": return new JsonArray();
            case "object":
                var result = new JsonObject();
                foreach (var field in Description.Fields(schema, isNew: true))
                    if (field.Required || field.Schema.TryGetProperty("default", out _)) result[field.Key] = Initial(field.Schema, false, depth + 1);
                return result;
            case "boolean": return JsonValue.Create(false);
            case "integer": return JsonValue.Create(schema.TryGetProperty("minimum", out var integer) ? Convert.ToInt64(decimal.Ceiling(integer.GetDecimal())) : 0);
            case "number": return JsonValue.Create(schema.TryGetProperty("minimum", out var number) ? number.GetDouble() : 0.0);
            default: return JsonValue.Create("");
        }
    }
    public string Preview() => PreviewNode(Description.Root, value, 0)?.ToJsonString(new JsonSerializerOptions { WriteIndented = true }) ?? "null";
    private JsonNode? PreviewNode(JsonElement raw, JsonNode? node, int depth)
    {
        if (depth > 32) throw new InvalidOperationException("Configuration is too deep");
        var schema = Description.Resolve(raw);
        if (Hidden(schema) || Flag(schema, "sensitive") || Metadata(schema, "export") == "omit") return null;
        if (node is null) return null;
        schema = Active(schema, node);
        if (node is JsonArray array) {
            var result = new JsonArray();
            foreach (var item in array) result.Add(PreviewNode(schema.GetProperty("items"), item, depth + 1));
            return result;
        }
        if (node is JsonObject record) {
            var result = new JsonObject();
            if (!schema.TryGetProperty("properties", out var properties)) return result;
            foreach (var property in properties.EnumerateObject()) {
                var child = Description.Resolve(property.Value);
                if (Hidden(child) || Flag(child, "sensitive") || Metadata(child, "export") == "omit" || !record.ContainsKey(property.Name)) continue;
                result[property.Name] = PreviewNode(child, record[property.Name], depth + 1);
            }
            return result;
        }
        return node.DeepClone();
    }
    public JsonElement ParseScalar(JsonElement raw, string text)
    {
        var schema = Active(Description.Resolve(raw), null);
        JsonElement value;
        switch (Type(schema)) {
            case "integer":
                if (!long.TryParse(text, NumberStyles.Integer, CultureInfo.InvariantCulture, out var integer)) throw new FormatException("Enter an integer");
                value = JsonSerializer.SerializeToElement(integer); break;
            case "number":
                if (!double.TryParse(text, NumberStyles.Float, CultureInfo.InvariantCulture, out var number) || double.IsNaN(number) || double.IsInfinity(number))
                    throw new FormatException("Enter a finite number using a decimal point");
                value = JsonSerializer.SerializeToElement(number); break;
            case "boolean":
                if (!bool.TryParse(text, out var boolean)) throw new FormatException("Choose true or false");
                value = JsonSerializer.SerializeToElement(boolean); break;
            default:
                var length = 0;
                for (int i = 0; i < text.Length; i++, length++) {
                    if (char.IsHighSurrogate(text[i])) {
                        if (++i >= text.Length || !char.IsLowSurrogate(text[i])) throw new FormatException("Invalid Unicode text");
                    } else if (char.IsLowSurrogate(text[i])) throw new FormatException("Invalid Unicode text");
                }
                if ((schema.TryGetProperty("minLength", out var minimumLength) && length < minimumLength.GetInt32()) ||
                    (schema.TryGetProperty("maxLength", out var maximumLength) && length > maximumLength.GetInt32())) throw new FormatException("Text length is outside the described range");
                if (schema.TryGetProperty("enum", out _) && !Description.Choices(schema).Any(c => c.Enabled && c.Value == text)) throw new FormatException("Choose an available value");
                if (Text(schema, "format") == "uuid" && !Guid.TryParse(text, out _)) throw new FormatException("Choose a valid device identity");
                value = JsonSerializer.SerializeToElement(text); break;
        }
        if (value.ValueKind == JsonValueKind.Number) {
            var number = value.GetDouble();
            if ((schema.TryGetProperty("minimum", out var minimum) && number < minimum.GetDouble()) ||
                (schema.TryGetProperty("maximum", out var maximum) && number > maximum.GetDouble())) throw new FormatException("Number is outside the described range");
            if ((Text(schema, "format") == "uint32" && (number < 0 || number > uint.MaxValue)) ||
                (Text(schema, "format") == "int32" && (number < int.MinValue || number > int.MaxValue)))
                throw new FormatException("Integer is outside the described type range");
        }
        return value;
    }
    private HubDraftField Editable(string path)
    {
        if (path.Length == 0) throw new InvalidOperationException("The configuration root cannot be replaced");
        var field = Field(path);
        if (field.ReadOnly || !field.Enabled) throw new InvalidOperationException("This configuration field is read-only or unavailable");
        return field;
    }
    internal JsonElement Active(JsonElement raw, JsonNode? node)
    {
        var schema = Description.Resolve(raw);
        if (schema.TryGetProperty("anyOf", out var nullable)) schema = Description.Resolve(nullable.EnumerateArray().First(n => Type(n) != "null"));
        if (schema.TryGetProperty("oneOf", out _)) {
            var scalar = Description.Choices(schema);
            if (scalar.Count != 0) {
                var merged = JsonNode.Parse(schema.GetRawText())!.AsObject(); merged.Remove("oneOf");
                merged["type"] = "string"; merged["enum"] = JsonSerializer.SerializeToNode(scalar.Select(c => c.Value));
                var metadata = merged["x-regain"]?.AsObject() ?? new JsonObject();
                var rules = metadata["enumCapabilities"]?.AsObject() ?? new JsonObject();
                foreach (var alternative in schema.GetProperty("oneOf").EnumerateArray()) {
                    var alternativeSchema = Description.Resolve(alternative); var capability = Metadata(alternativeSchema, "requiresCapability");
                    if (capability.Length != 0) rules[Text(alternativeSchema, "const")] = capability;
                }
                metadata["enumCapabilities"] = rules.DeepClone(); merged["x-regain"] = metadata.DeepClone(); return Element(merged);
            }
            var kind = node is JsonObject record ? record["kind"]?.GetValue<string>() : null;
            var choice = Description.Variants(schema).SingleOrDefault(c => c.Kind == kind) ?? throw new InvalidOperationException("Select a configuration type");
            schema = Description.Resolve(choice.Schema);
        }
        return schema;
    }
    private JsonNode? Node(string path)
    {
        JsonNode? node = value;
        foreach (var part in Parts(path)) node = node is JsonArray array ? array[Index(part, array.Count)] : ((JsonObject)node!)[part];
        return node;
    }
    private (JsonNode Parent, string Key) Parent(string path)
    {
        var parts = Parts(path); if (parts.Length == 0) throw new InvalidOperationException("Missing field path");
        JsonNode? parent = value;
        for (int i = 0; i < parts.Length - 1; i++) parent = parent is JsonArray array ? array[Index(parts[i], array.Count)] : ((JsonObject)parent!)[parts[i]];
        return (parent ?? throw new InvalidOperationException("Missing field parent"), parts[parts.Length - 1]);
    }
    private static string[] Parts(string path)
    {
        if (path == "") return [];
        if (!path.StartsWith("/", StringComparison.Ordinal) || path.Length > 32768) throw new InvalidOperationException("Invalid configuration field path");
        return path.Substring(1).Split('/').Select(p => p.Replace("~1", "/").Replace("~0", "~")).ToArray();
    }
    private static int Index(string key, int count) => int.TryParse(key, NumberStyles.None, CultureInfo.InvariantCulture, out var index) &&
        index >= 0 && index < count && key == index.ToString(CultureInfo.InvariantCulture) ? index : throw new InvalidOperationException("Invalid collection index");
    internal static JsonElement Element(JsonNode? node) => JsonSerializer.SerializeToElement(node);
    internal static string Type(JsonElement schema) => schema.TryGetProperty("type", out var type) ? type.ValueKind == JsonValueKind.Array
        ? type.EnumerateArray().Select(n => n.GetString()!).First(n => n != "null") : type.GetString()! : "";
    internal static string Text(JsonElement schema, string key) => schema.TryGetProperty(key, out var property) && property.ValueKind == JsonValueKind.String ? property.GetString()! : "";
    internal static string Metadata(JsonElement schema, string key) => schema.TryGetProperty("x-regain", out var metadata) ? Text(metadata, key) : "";
    internal static bool Flag(JsonElement schema, string key) => schema.TryGetProperty("x-regain", out var metadata) && metadata.TryGetProperty(key, out var flag) && flag.ValueKind == JsonValueKind.True;
    private static bool Hidden(JsonElement schema) => Flag(schema, "hidden");
    private static bool ReadOnly(JsonElement schema, bool prior) => (schema.TryGetProperty("readOnly", out var flag) && flag.ValueKind == JsonValueKind.True) || (prior && Flag(schema, "immutableAfterCreate"));
}

public sealed class HubDraftField(string path, JsonElement schema, JsonElement? value, JsonElement? original, bool required, bool readOnly, bool enabled)
{
    public string Path { get; } = path;
    public JsonElement Schema { get; } = schema;
    public JsonElement? Value { get; } = value;
    public JsonElement? Original { get; } = original;
    public bool Required { get; } = required;
    public bool ReadOnly { get; } = readOnly;
    public bool Enabled { get; } = enabled;
}
