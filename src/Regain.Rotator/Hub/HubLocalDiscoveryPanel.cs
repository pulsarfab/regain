using System.Text.Json;
using System.Windows;
using System.Windows.Controls;

namespace Regain.Hub;

public sealed partial class HubConfigurationWindow
{
    private void RenderLocalDiscovery()
    {
        var d = session!.LocalDiscoveryDescription;
        var definitions = d.GetProperty("targetSchema").GetProperty("$defs");
        discoveryPanel.Children.Add(Text("Find devices on this host", 22));
        var kind = new ComboBox { Margin = new Thickness(4), Tag = "local-discovery-kind" };
        foreach (var key in new[] { "native", "com" }) kind.Items.Add(new ComboBoxItem {
            Tag = key, Content = d.GetProperty(key).GetProperty("label").GetString(),
            IsEnabled = session.Description!.Value.GetProperty("capabilities").EnumerateArray().Any(c => c.GetString() == d.GetProperty(key).GetProperty("requiresCapability").GetString())
        });
        discoveryPanel.Children.Add(kind);
        var hint = Text(""); discoveryPanel.Children.Add(hint);
        var native = Choices("NativeDevice", "Native backend", "local-discovery-device");
        var type = Choices("DeviceType", "ASCOM device class", "local-discovery-type");
        var bitness = Choices("Bitness", "ASCOM architecture", "local-discovery-bitness");
        var query = CredentialButton("Read local device catalog"); discoveryPanel.Children.Add(query);
        var result = new TextBox { IsReadOnly = true, TextWrapping = TextWrapping.Wrap, AcceptsReturn = true,
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto, MinHeight = 110, MaxHeight = 230, Tag = "local-discovery-result", Margin = new Thickness(4) };
        discoveryPanel.Children.Add(result);
        var selection = new ComboBox { Margin = new Thickness(4), Tag = "local-discovery-selection" }; discoveryPanel.Children.Add(selection);
        var add = CredentialButton("Add local source to draft"); add.IsEnabled = false; discoveryPanel.Children.Add(add);
        foreach (var box in new[] { native.box, type.box, bitness.box }) box.SelectionChanged += (_, _) => {
            selection.Items.Clear(); result.Clear(); add.IsEnabled = false;
        };
        selection.SelectionChanged += (_, _) => add.IsEnabled = selection.SelectedItem is ComboBoxItem item && item.IsEnabled;
        kind.SelectionChanged += (_, _) => {
            var selected = kind.SelectedItem as ComboBoxItem; var key = (string?)selected?.Tag;
            query.IsEnabled = selected?.IsEnabled == true;
            hint.Text = key is null ? "" : d.GetProperty(key).GetProperty("description").GetString()!;
            native.panel.Visibility = key == "native" ? Visibility.Visible : Visibility.Collapsed;
            type.panel.Visibility = bitness.panel.Visibility = key == "com" ? Visibility.Visible : Visibility.Collapsed;
            selection.Items.Clear(); result.Clear(); add.IsEnabled = false;
        };
        kind.SelectedIndex = 0;
        query.Click += async (_, _) => await Run(async () => {
            selection.Items.Clear(); result.Clear(); add.IsEnabled = false;
            if (kind.SelectedItem is not ComboBoxItem selected || !selected.IsEnabled) throw new InvalidOperationException("Select an available discovery backend");
            var key = (string)selected.Tag;
            var target = key == "native" ? JsonSerializer.SerializeToElement(new { kind = key, device = (string)native.box.SelectedItem }) :
                JsonSerializer.SerializeToElement(new { kind = key, deviceType = (string)type.box.SelectedItem, bitness = (string)bitness.box.SelectedItem });
            var catalog = await session.DiscoverLocalAsync(target, lifetime.Token); var lines = new List<string>();
            if (catalog.GetProperty("simulated").GetBoolean()) lines.Add("SIMULATION — generated identities." +
                (key == "native" && target.GetProperty("device").GetString() == "camera-direct" ? " Direct camera models are alternatives for one simulated serial." : ""));
            if (catalog.GetProperty("incomplete").GetBoolean()) lines.Add("Catalog incomplete: a probe, registration or scan limit prevented complete results.");
            var index = 0;
            foreach (var entry in catalog.GetProperty("entries").EnumerateArray()) {
                var backend = entry.GetProperty("backend"); var blocked = entry.GetProperty("blockedReason");
                var reason = blocked.ValueKind == JsonValueKind.Null ? "" : d.GetProperty("blockedReasons").GetProperty(blocked.GetString()!).GetString();
                var identity = backend.GetProperty(key == "native" ? "identity" : "progId").GetString();
                lines.Add(entry.GetProperty("name").GetString() + "\nID: " + identity + (reason == "" ? "" : "\n" + reason));
                selection.Items.Add(new ComboBoxItem { Content = entry.GetProperty("name").GetString() + " — " + identity,
                    Tag = index++, IsEnabled = blocked.ValueKind == JsonValueKind.Null });
            }
            result.Text = lines.Count == 0 ? "No matching devices or registrations were reported." : string.Join("\n\n", lines);
            status.Text = "Read " + index + " local catalog entries. Configuration is unchanged; no source lease was created.";
        });
        add.Click += async (_, _) => await Run(() => {
            if (selection.SelectedItem is not ComboBoxItem item || !item.IsEnabled) throw new InvalidOperationException("Select an available catalog entry");
            var id = session.AddDiscoveredLocalSource((int)item.Tag);
            form!.Render(); preview.Clear(); errors.Text = "";
            status.Text = "Added source " + id + " to the draft. Review settings and apply before connecting equipment.";
            return Task.CompletedTask;
        });
        (StackPanel panel, ComboBox box) Choices(string definition, string label, string tag) {
            var panel = new StackPanel(); panel.Children.Add(Text(label));
            var box = new ComboBox { Margin = new Thickness(4), Tag = tag };
            foreach (var value in definitions.GetProperty(definition).GetProperty("enum").EnumerateArray()) box.Items.Add(value.GetString()!);
            box.SelectedIndex = 0; panel.Children.Add(box); discoveryPanel.Children.Add(panel); return (panel, box);
        }
    }
}
