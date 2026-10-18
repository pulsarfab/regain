using System.Globalization;
using System.Text.Json;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;

namespace Regain.Rotator;

// Rust owns the keys, defaults, descriptions and limits used by every frontend.
public static class FocuserCompensationConfiguration
{
    public static JsonElement Schema { get; } = Load();
    private static JsonElement Load()
    {
        using var stream = typeof(FocuserCompensationConfiguration).Assembly.GetManifestResourceStream("Regain.FocuserCompensation.json")
            ?? throw new InvalidOperationException("Missing focuser compensation contract");
        using var document = JsonDocument.Parse(stream);
        return document.RootElement.Clone();
    }
    public static JsonElement Parse(JsonProperty field, string text)
    {
        var schema = field.Value;
        var type = schema.GetProperty("type").GetString();
        object value;
        if (type == "string" && schema.GetProperty("enum").EnumerateArray().Any(v => v.GetString() == text)) value = text;
        else if (type == "boolean" && bool.TryParse(text, out var flag)) value = flag;
        else if (type is "number" or "integer" && double.TryParse(text, NumberStyles.Float, CultureInfo.InvariantCulture, out var number)
            && !double.IsNaN(number) && !double.IsInfinity(number)
            && (type != "integer" || number == Math.Truncate(number))
            && number >= schema.GetProperty("minimum").GetDouble() && number <= schema.GetProperty("maximum").GetDouble())
            value = type == "integer" ? (object)(int)number : number;
        else throw new ArgumentException("Invalid " + schema.GetProperty("title").GetString());
        return JsonSerializer.SerializeToElement(value);
    }
    public static void Validate(Dictionary<string, JsonElement> values)
    {
        var properties = Schema.GetProperty("properties");
        foreach (var item in values) {
            if (!properties.TryGetProperty(item.Key, out var field)) throw new ArgumentException("Unknown compensation setting " + item.Key);
            string type = field.GetProperty("type").GetString()!;
            if ((type == "boolean" && item.Value.ValueKind is not (JsonValueKind.True or JsonValueKind.False)) ||
                (type == "string" && item.Value.ValueKind != JsonValueKind.String) ||
                (type is "integer" or "number" && item.Value.ValueKind != JsonValueKind.Number)) throw new ArgumentException("Wrong type for " + item.Key);
            var property = properties.EnumerateObject().Single(p => p.Name == item.Key);
            Parse(property, type == "string" ? item.Value.GetString()! : item.Value.ToString());
        }
    }
}

public sealed class FocuserCompensationForm
{
    private readonly Dictionary<string, Func<JsonElement>> readers = [];
    public FocuserCompensationForm(Panel panel, Dictionary<string, JsonElement> values)
    {
        foreach (var field in FocuserCompensationConfiguration.Schema.GetProperty("properties").EnumerateObject()) {
            var schema = field.Value;
            var value = values.TryGetValue(field.Name, out var saved) ? saved : schema.GetProperty("default");
            var title = schema.GetProperty("title").GetString()!;
            panel.Children.Add(new TextBlock { Text = title, TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 12, 0, 4) });
            Control editor;
            if (schema.GetProperty("type").GetString() == "boolean") {
                var toggle = new CheckBox { IsChecked = value.GetBoolean() }; editor = toggle;
                readers.Add(field.Name, () => FocuserCompensationConfiguration.Parse(field, (toggle.IsChecked == true).ToString()));
            } else if (schema.TryGetProperty("enum", out var choices)) {
                var select = new ComboBox { ItemsSource = choices.EnumerateArray().Select(v => v.GetString()).ToArray(), SelectedItem = value.GetString(), MinWidth = 180, HorizontalAlignment = HorizontalAlignment.Left }; editor = select;
                readers.Add(field.Name, () => FocuserCompensationConfiguration.Parse(field, select.SelectedItem as string ?? ""));
            } else {
                var input = new TextBox { Text = value.GetRawText(), Width = 180 }; editor = input;
                readers.Add(field.Name, () => FocuserCompensationConfiguration.Parse(field, input.Text.Trim()));
            }
            AutomationProperties.SetName(editor, title);
            AutomationProperties.SetAutomationId(editor, "focuser-compensation-" + field.Name);
            editor.ToolTip = schema.GetProperty("description").GetString();
            panel.Children.Add(editor);
            panel.Children.Add(new TextBlock { Text = schema.GetProperty("description").GetString(), TextWrapping = TextWrapping.Wrap, FontSize = 12, Opacity = .8 });
        }
    }
    public Dictionary<string, JsonElement> Read() => readers.ToDictionary(r => r.Key, r => r.Value());
}
