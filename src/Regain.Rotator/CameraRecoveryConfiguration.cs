using System.Globalization;
using System.Text.Json;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;

namespace Regain.Rotator;

// The same Rust-generated metadata drives standalone ASCOM, NINA and Alpaca.
public static class CameraRecoveryConfiguration
{
    public static JsonElement Schema { get; } = Load();
    public static IReadOnlyList<CameraRecoveryField> Fields { get; } = Schema.GetProperty("properties")
        .EnumerateObject().Select(p => new CameraRecoveryField(p.Name, p.Value.Clone()))
        .OrderBy(f => f.Schema.GetProperty("x-regain").GetProperty("order").GetInt32()).ToArray();

    private static JsonElement Load()
    {
        using var stream = typeof(CameraRecoveryConfiguration).Assembly.GetManifestResourceStream("Regain.CameraRecovery.json")
            ?? throw new InvalidOperationException("Missing camera recovery contract");
        using var document = JsonDocument.Parse(stream);
        if (document.RootElement.GetProperty("contractVersion").GetInt32() != 1)
            throw new InvalidOperationException("Unsupported camera recovery contract");
        return document.RootElement.GetProperty("schema").Clone();
    }
}

public sealed class CameraRecoveryField(string key, JsonElement schema)
{
    public string Key { get; } = key;
    public JsonElement Schema { get; } = schema;
    public string Title => Schema.GetProperty("title").GetString()!;
    public string Description => Schema.GetProperty("description").GetString()!;
    public string Units => Schema.GetProperty("x-regain").GetProperty("units").GetString()!;
    public string Label => Units.Length == 0 ? Title : $"{Title} ({Units})";
    public string Section => Schema.GetProperty("x-regain").GetProperty("section").GetString()!;
    public string Type => Schema.GetProperty("type").GetString()!;
    public JsonElement Default => Schema.GetProperty("default").Clone();
    public bool Available(string platform) => Schema.GetProperty("x-regain").GetProperty("platforms")
        .EnumerateArray().Any(p => p.GetString() == platform);
    public JsonElement Value(JsonElement values) => values.TryGetProperty(Key, out var value) ? value.Clone() : Default;

    public JsonElement Parse(string text)
    {
        object value;
        if (Type == "boolean") {
            if (!bool.TryParse(text, out var flag)) throw Invalid();
            value = flag;
        } else {
            if (!double.TryParse(text, NumberStyles.Float, CultureInfo.InvariantCulture, out var number) ||
                double.IsNaN(number) || double.IsInfinity(number) ||
                (Type == "integer" && (number != Math.Truncate(number) || number < int.MinValue || number > int.MaxValue)) ||
                (Schema.TryGetProperty("minimum", out var minimum) && number < minimum.GetDouble()) ||
                (Schema.TryGetProperty("exclusiveMinimum", out var exclusive) && number <= exclusive.GetDouble()) ||
                (Schema.TryGetProperty("maximum", out var maximum) && number > maximum.GetDouble()) ||
                (Schema.TryGetProperty("enum", out var choices) && !choices.EnumerateArray().Any(choice => choice.GetDouble() == number))) throw Invalid();
            value = Type == "integer" ? (object)(int)number : number;
        }
        return JsonSerializer.SerializeToElement(value);
    }
    private ArgumentException Invalid() => new($"Invalid {Title.ToLowerInvariant()}. {Description}");
}

// Caller supplies its existing tabs, styling and save policy. Reading is inert;
// it preserves hidden platform settings and legacy extension keys.
public sealed class CameraRecoveryForm
{
    private readonly JsonElement original;
    private readonly Dictionary<CameraRecoveryField, Func<JsonElement>> readers = [];
    public IReadOnlyList<Control> Editors { get; }

    public CameraRecoveryForm(JsonElement values, Func<string, Panel> section, Action? changed = null)
    {
        original = values.Clone();
        var editors = new List<Control>();
        foreach (var field in CameraRecoveryConfiguration.Fields.Where(f => f.Available("windows"))) {
            var body = section(field.Section);
            var row = new StackPanel { Margin = new Thickness(0, 0, 0, 14) };
            row.Children.Add(new TextBlock { Text = field.Label, TextWrapping = TextWrapping.Wrap });
            Control editor;
            if (field.Type == "boolean") {
                var toggle = new CheckBox { IsChecked = field.Value(values).GetBoolean(), HorizontalAlignment = HorizontalAlignment.Left };
                toggle.Click += (_, _) => changed?.Invoke();
                readers.Add(field, () => field.Parse((toggle.IsChecked == true).ToString()));
                editor = toggle;
            } else {
                var input = new TextBox { Text = field.Value(values).GetRawText(), MinHeight = 28,
                    VerticalContentAlignment = VerticalAlignment.Center, Margin = new Thickness(0, 4, 0, 4) };
                input.LostKeyboardFocus += (_, _) => changed?.Invoke();
                readers.Add(field, () => field.Parse(input.Text.Trim()));
                editor = input;
            }
            editor.ToolTip = field.Description;
            AutomationProperties.SetName(editor, field.Label);
            AutomationProperties.SetAutomationId(editor, "recovery-" + field.Key);
            row.Children.Add(editor);
            row.Children.Add(new TextBlock { Text = field.Description, TextWrapping = TextWrapping.Wrap,
                FontSize = 12, Opacity = .8 });
            editors.Add(editor);
            body.Children.Add(row);
        }
        Editors = editors;
    }

    public JsonElement Read()
    {
        var values = original.EnumerateObject().ToDictionary(p => p.Name, p => p.Value.Clone(), StringComparer.Ordinal);
        foreach (var pair in readers) values[pair.Key.Key] = pair.Value();
        return JsonSerializer.SerializeToElement(values);
    }
}
