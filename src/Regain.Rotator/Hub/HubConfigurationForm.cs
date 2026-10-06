using System.Globalization;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;

namespace Regain.Hub;

/// Native controls consume the same generated descriptors as web setup. No
/// source policy labels, defaults or bounds are declared in this renderer.
internal sealed class HubConfigurationForm(HubConfigurationDraft draft, Action changed, Action<string> failed)
{
    private readonly Dictionary<string, string> errors = new(StringComparer.Ordinal);
    private readonly Dictionary<string, string> invalidInput = new(StringComparer.Ordinal);
    private readonly HashSet<string> expanded = new(StringComparer.Ordinal);
    internal StackPanel Root { get; } = new();
    internal IReadOnlyDictionary<string, string> Errors => errors;
    internal void Render()
    {
        Root.Children.Clear(); Root.Children.Add(Field("", "Hub configuration", 0));
    }
    private UIElement Field(string path, string? title, int depth)
    {
        if (depth > 32) throw new InvalidOperationException("Configuration is too deep");
        var field = draft.Field(path); var schema = field.Schema;
        var panel = new StackPanel { Margin = new Thickness(8, 5, 8, 5), IsEnabled = field.Enabled };
        panel.Children.Add(new TextBlock { Text = title ?? HubConfigurationDraft.Text(schema, "title"), FontWeight = FontWeights.SemiBold });
        var description = HubConfigurationDraft.Text(schema, "description");
        if (description.Length != 0) panel.Children.Add(Note(description));
        if (field.ReadOnly) {
            var text = HubConfigurationDraft.Flag(schema, "sensitive") ? "Protected value" :
                field.Value?.ValueKind == JsonValueKind.String ? field.Value.Value.GetString()! : field.Value?.GetRawText() ?? "Not set";
            if (HubConfigurationDraft.Text(schema, "format") == "uuid") {
                var identity = Lazy(path, title ?? "Identity", () => {
                    var details = new StackPanel(); if (description.Length != 0) details.Children.Add(Note(description)); details.Children.Add(Note(text)); return details;
                }, true);
                identity.IsEnabled = field.Enabled; return identity;
            }
            panel.Children.Add(Note(text)); return panel;
        }
        if (!field.Required && !field.Value.HasValue) {
            panel.Children.Add(Button("Add / customize", () => Change(() => draft.AddOptional(path)))); return panel;
        }
        if (!field.Required) panel.Children.Add(Button("Remove optional value", () => Change(() => draft.RemoveOptional(path))));
        if (field.Value?.ValueKind == JsonValueKind.Null) {
            panel.Children.Add(Note("Not set"));
            panel.Children.Add(Button("Set value", () => Change(() => draft.AddOptional(path, replaceNull: true)))); return panel;
        }
        var variants = draft.Description.Variants(schema);
        if (variants.Count != 0) {
            var selection = new ComboBox { MinWidth = 240, HorizontalAlignment = HorizontalAlignment.Left, Tag = path };
            foreach (var choice in variants) selection.Items.Add(new ComboBoxItem { Content = choice.Title + (choice.Enabled ? "" : " (not available)"), Tag = choice.Kind,
                ToolTip = choice.Description, IsEnabled = choice.Enabled });
            selection.SelectedItem = selection.Items.Cast<ComboBoxItem>().FirstOrDefault(c => (string)c.Tag == field.Value?.GetProperty("kind").GetString());
            selection.SelectionChanged += (_, _) => Try(() => { if (selection.SelectedItem is ComboBoxItem choice) Change(() => draft.SelectVariant(path, (string)choice.Tag)); });
            panel.Children.Add(selection);
            schema = draft.Active(schema, field.Value.HasValue ? JsonNode.Parse(field.Value.Value.GetRawText()) : null);
            if (HubConfigurationDraft.Text(schema, "description") is { Length: > 0 } note) panel.Children.Add(Note(note));
        } else if (schema.TryGetProperty("anyOf", out _) || schema.TryGetProperty("oneOf", out _)) schema = draft.Active(schema, null);
        var type = HubConfigurationDraft.Type(schema);
        if (type == "array") { Array(panel, path, field.Value!.Value, depth); }
        else if (type == "object") {
            foreach (var descriptor in draft.Description.Fields(schema, field.Value, isNew: !field.Original.HasValue)) {
                if (descriptor.Schema.TryGetProperty("const", out _)) continue;
                var child = path + "/" + descriptor.Key.Replace("~", "~0").Replace("/", "~1");
                panel.Children.Add(Lazy(child, descriptor.Title, () => Field(child, descriptor.Title, depth + 1),
                    HubConfigurationDraft.Type(descriptor.Schema) is "array" or "object" || descriptor.Schema.TryGetProperty("oneOf", out _)));
            }
        } else Scalar(panel, path, schema, field);
        panel.IsEnabled = field.Enabled; return panel;
    }
    private void Array(Panel panel, string path, JsonElement values, int depth)
    {
        const int pageSize = 32;
        int page = 0;
        var list = new StackPanel(); var row = new StackPanel { Orientation = Orientation.Horizontal };
        Button previous = null!, next = null!;
        var summary = Note("");
        previous = Button("Previous", () => { page--; Draw(); });
        next = Button("Next", () => { page++; Draw(); });
        row.Children.Add(previous); row.Children.Add(next); row.Children.Add(summary);
        panel.Children.Add(row); panel.Children.Add(list);
        void Draw()
        {
            list.Children.Clear();
            var count = values.GetArrayLength(); var start = page * pageSize; var end = Math.Min(count, start + pageSize);
            previous.IsEnabled = page > 0; next.IsEnabled = end < count; summary.Text = count == 0 ? "No items" : $"{start + 1}–{end} of {count}";
            for (int i = start; i < end; i++) {
                var index = i; var item = values[i]; var child = path + "/" + index.ToString(CultureInfo.InvariantCulture);
                var label = item.ValueKind == JsonValueKind.Object && item.TryGetProperty("label", out var name) ? name.GetString()! : "Item " + (i + 1);
                list.Children.Add(Lazy(child, label, () => {
                    var content = new StackPanel(); content.Children.Add(Field(child, "Settings", depth + 1));
                    content.Children.Add(Button("Remove item", () => Change(() => draft.RemoveItem(path, index)))); return content;
                }, true));
            }
        }
        Draw();
        var add = Button("Add item", () => Change(() => draft.AddItem(path)));
        var schema = draft.Field(path).Schema;
        add.IsEnabled = !schema.TryGetProperty("maxItems", out var maximum) || values.GetArrayLength() < maximum.GetInt32(); panel.Children.Add(add);
    }
    private UIElement Lazy(string path, string title, Func<UIElement> render, bool lazy)
    {
        if (!lazy) return render();
        var box = new Expander { Header = title, Margin = new Thickness(0, 4, 0, 4) };
        box.Expanded += (_, args) => { if (ReferenceEquals(args.OriginalSource, box)) Try(() => { expanded.Add(path); box.Content ??= render(); }); };
        box.Collapsed += (_, args) => { if (ReferenceEquals(args.OriginalSource, box)) { expanded.Remove(path); box.Content = null; } };
        box.IsExpanded = expanded.Contains(path); return box;
    }
    private void Scalar(Panel panel, string path, JsonElement schema, HubDraftField field)
    {
        var error = new TextBlock { Foreground = Brushes.Firebrick, TextWrapping = TextWrapping.Wrap,
            Text = errors.TryGetValue(path, out var priorError) ? priorError : "" };
        void Set(string text)
        {
            try { draft.SetValue(path, draft.ParseScalar(schema, text)); errors.Remove(path); invalidInput.Remove(path); error.Text = ""; }
            catch (FormatException problem) { errors[path] = problem.Message; invalidInput[path] = text; error.Text = problem.Message; }
            catch { failed("This field cannot be changed in the current editor state."); return; }
            Try(changed);
        }
        var type = HubConfigurationDraft.Type(schema);
        var reference = HubConfigurationDraft.Metadata(schema, "reference");
        if (reference.Length != 0 || schema.TryGetProperty("enum", out _)) {
            var select = new ComboBox { MinWidth = 240, MaxWidth = 650, HorizontalAlignment = HorizontalAlignment.Left, Tag = path };
            select.Items.Add(new ComboBoxItem { Content = "Choose…", Tag = "" });
            if (reference.Length != 0) {
                if (reference is not ("source" or "output")) throw new InvalidOperationException("Unsupported reference descriptor");
                foreach (var entry in draft.Candidate.GetProperty(reference == "source" ? "sources" : "outputs").EnumerateArray())
                    select.Items.Add(new ComboBoxItem { Content = entry.GetProperty("label").GetString() + " (" + entry.GetProperty("id").GetString() + ")", Tag = entry.GetProperty("id").GetString() });
            } else {
                var choices = draft.Description.Choices(field.Schema);
                if (choices.Count == 0) choices = draft.Description.Choices(schema);
                foreach (var choice in choices) select.Items.Add(new ComboBoxItem { Content = choice.Value + (choice.Enabled ? "" : " (not available)"),
                    Tag = choice.Value, IsEnabled = choice.Enabled, ToolTip = choice.Description });
            }
            var current = invalidInput.TryGetValue(path, out var pending) ? pending : field.Value?.GetString() ?? "";
            if (!select.Items.Cast<ComboBoxItem>().Any(c => (string?)c.Tag == current)) select.Items.Add(new ComboBoxItem { Content = "Unavailable: " + current, Tag = current, IsEnabled = false });
            select.SelectedItem = select.Items.Cast<ComboBoxItem>().First(c => (string?)c.Tag == current);
            select.SelectionChanged += (_, _) => { if (select.SelectedItem is ComboBoxItem item) Set((string)item.Tag); };
            panel.Children.Add(select);
        } else if (type == "boolean") {
            var input = new CheckBox { Content = "Enabled", Tag = path, IsChecked = field.Value?.GetBoolean() ?? false };
            input.Click += (_, _) => Set((input.IsChecked == true).ToString()); panel.Children.Add(input);
        } else if (HubConfigurationDraft.Flag(schema, "sensitive")) {
            var input = new PasswordBox { MinWidth = 240, MaxWidth = 650, Tag = path,
                Password = invalidInput.TryGetValue(path, out var pending) ? pending : field.Value?.GetString() ?? "", HorizontalAlignment = HorizontalAlignment.Left };
            input.PasswordChanged += (_, _) => Set(input.Password); panel.Children.Add(input);
        } else {
            var input = new TextBox { MinWidth = 240, MaxWidth = 650, Tag = path, HorizontalAlignment = HorizontalAlignment.Left,
                Text = invalidInput.TryGetValue(path, out var pending) ? pending : field.Value?.ValueKind == JsonValueKind.String ? field.Value.Value.GetString()! : field.Value?.GetRawText() ?? "" };
            input.TextChanged += (_, _) => Set(input.Text); panel.Children.Add(input);
        }
        var hints = new List<string>();
        if (schema.TryGetProperty("minimum", out var minimum)) hints.Add("Minimum " + minimum.GetRawText());
        if (schema.TryGetProperty("maximum", out var maximum)) hints.Add("Maximum " + maximum.GetRawText());
        if (schema.TryGetProperty("x-regain", out var metadata) && metadata.TryGetProperty("step", out var step)) hints.Add("Step " + step.GetRawText());
        var units = HubConfigurationDraft.Metadata(schema, "units"); if (units.Length != 0) hints.Add(units);
        if (hints.Count != 0) panel.Children.Add(Note(string.Join(" · ", hints)));
        panel.Children.Add(error);
    }
    private void Change(Action mutation)
    {
        if (errors.Count != 0) throw new InvalidOperationException("Correct the highlighted input before changing the form structure");
        mutation(); changed(); Render();
    }
    private static TextBlock Note(string text) => new() { Text = text, TextWrapping = TextWrapping.Wrap, Margin = new Thickness(0, 3, 0, 3) };
    private void Try(Action action) { try { action(); } catch (InvalidOperationException error) { failed(error.Message); } catch { failed("Could not update the configuration form."); } }
    private Button Button(string text, Action action)
    {
        var button = new Button { Content = text, Padding = new Thickness(10, 6, 10, 6), Margin = new Thickness(3), HorizontalAlignment = HorizontalAlignment.Left };
        button.Click += (_, _) => Try(action); return button;
    }
}
