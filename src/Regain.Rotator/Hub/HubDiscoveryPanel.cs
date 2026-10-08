using System.Text.Json;
using System.Windows;
using System.Windows.Controls;

namespace Regain.Hub;

public sealed partial class HubConfigurationWindow
{
    private readonly StackPanel discoveryPanel = new() { Margin = new Thickness(8) };
    private TextBox? discoveryUrl, discoveryCredential, discoveryScope;
    private void RenderDiscovery()
    {
        discoveryPanel.Children.Clear();
        var description = session!.DiscoveryDescription;
        var parameters = description.GetProperty("parameters");
        var network = session.NetworkDiscoveryDescription;
        discoveryPanel.Children.Add(Text(network.GetProperty("label").GetString()!, 22));
        discoveryPanel.Children.Add(Text(network.GetProperty("description").GetString()!));
        var search = CredentialButton("Find Alpaca servers"); discoveryPanel.Children.Add(search);
        var candidates = new ComboBox { Margin = new Thickness(4), Tag = "network-discovery-selection" }; discoveryPanel.Children.Add(candidates);
        var choose = CredentialButton("Use selected server address"); choose.IsEnabled = false; discoveryPanel.Children.Add(choose);
        var searchStatus = Text(""); discoveryPanel.Children.Add(searchStatus);
        candidates.SelectionChanged += (_, _) => choose.IsEnabled = candidates.SelectedItem is ComboBoxItem item && item.IsEnabled;
        search.Click += async (_, _) => await Run(async () => {
            candidates.Items.Clear(); choose.IsEnabled = false; searchStatus.Text = "";
            var found = await session.SearchAlpacaAsync(lifetime.Token);
            foreach (var server in found.GetProperty("servers").EnumerateArray()) {
                var scope = server.GetProperty("scopeId").GetUInt32();
                candidates.Items.Add(new ComboBoxItem { Tag = server.Clone(),
                    Content = server.GetProperty("baseUrl").GetString() + (scope == 0 ? "" : " (interface " + scope + ")") });
            }
            searchStatus.Text = found.GetProperty("servers").GetArrayLength() + " server candidates. " +
                (found.GetProperty("incomplete").GetBoolean() ? "Search was incomplete; an interface failed or a limit was reached. " : "") +
                "No catalog was read and no equipment connection was opened.";
        });
        discoveryPanel.Children.Add(Text("Find devices on an Alpaca server", 22));
        discoveryPanel.Children.Add(Text("Read the server's device catalog without connecting equipment. Select a supported device to add a source to your draft, then review its settings and apply."));
        var url = parameters.GetProperty("baseUrl");
        discoveryPanel.Children.Add(Text(url.GetProperty("label").GetString()!));
        discoveryUrl = new TextBox { Margin = new Thickness(4), MaxLength = url.GetProperty("maxLength").GetInt32(), Tag = "discovery-url" };
        discoveryPanel.Children.Add(discoveryUrl);
        var scopeParameter = parameters.GetProperty("scopeId");
        discoveryPanel.Children.Add(Text(scopeParameter.GetProperty("label").GetString()!));
        discoveryPanel.Children.Add(Text(scopeParameter.GetProperty("description").GetString()!));
        discoveryScope = new TextBox { Margin = new Thickness(4), Tag = "discovery-scope" }; discoveryPanel.Children.Add(discoveryScope);
        var credential = parameters.GetProperty("credentialReference");
        discoveryPanel.Children.Add(Text(credential.GetProperty("label").GetString()!));
        discoveryPanel.Children.Add(Text(credential.GetProperty("description").GetString()!));
        discoveryCredential = new TextBox { Margin = new Thickness(4), Tag = "discovery-credential" }; discoveryPanel.Children.Add(discoveryCredential);
        discoveryPanel.Children.Add(Text($"One query, up to {description.GetProperty("timeoutSeconds").GetInt32()} seconds and {description.GetProperty("maximumDevices").GetInt32()} devices. Enter the server URL, including any reverse-proxy prefix."));
        var query = CredentialButton("Read Alpaca device catalog"); discoveryPanel.Children.Add(query);
        var result = new TextBox { IsReadOnly = true, TextWrapping = TextWrapping.Wrap, AcceptsReturn = true,
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto, MinHeight = 180, Tag = "discovery-result", Margin = new Thickness(4) };
        discoveryPanel.Children.Add(result);
        var selection = new ComboBox { Margin = new Thickness(4), Tag = "discovery-selection" }; discoveryPanel.Children.Add(selection);
        var add = CredentialButton("Add selected source to draft"); add.IsEnabled = false; discoveryPanel.Children.Add(add);
        selection.SelectionChanged += (_, _) => add.IsEnabled = selection.SelectedItem is ComboBoxItem item && item.IsEnabled;
        choose.Click += (_, _) => {
            if (candidates.SelectedItem is not ComboBoxItem item || !item.IsEnabled) return;
            var server = (JsonElement)item.Tag; var scope = server.GetProperty("scopeId").GetUInt32();
            discoveryUrl!.Text = server.GetProperty("baseUrl").GetString()!;
            discoveryScope!.Text = scope == 0 ? "" : scope.ToString(System.Globalization.CultureInfo.InvariantCulture);
            discoveryCredential!.Clear();
            result.Clear(); selection.Items.Clear(); add.IsEnabled = false;
            status.Text = "Server address selected. Choose a credential reference if needed, then read its catalog.";
        };
        add.Click += async (_, _) => await Run(() => {
            if (selection.SelectedItem is not ComboBoxItem item || !item.IsEnabled) throw new InvalidOperationException("Select a supported catalog entry");
            var id = session!.AddDiscoveredAlpacaSource((int)item.Tag);
            form!.Render(); preview.Clear(); errors.Text = "";
            status.Text = "Added pinned source " + id + " to the draft. Review its settings and apply before creating output connections.";
            return Task.CompletedTask;
        });
        query.Click += async (_, _) => await Run(async () => {
            result.Clear(); selection.Items.Clear(); add.IsEnabled = false;
            uint? scope = null;
            if (!string.IsNullOrWhiteSpace(discoveryScope.Text)) {
                if (!uint.TryParse(discoveryScope.Text, System.Globalization.NumberStyles.None,
                    System.Globalization.CultureInfo.InvariantCulture, out var entered) ||
                    entered < scopeParameter.GetProperty("minimum").GetUInt32() || entered > scopeParameter.GetProperty("maximum").GetUInt32())
                    throw new InvalidOperationException("Enter a valid IPv6 interface index or leave it empty");
                scope = entered;
            }
            var catalog = await session!.DiscoverAlpacaScopedAsync(discoveryUrl.Text, scope,
                string.IsNullOrWhiteSpace(discoveryCredential.Text) ? null : discoveryCredential.Text, lifetime.Token);
            result.Text = string.Join("\n\n", catalog.GetProperty("devices").EnumerateArray().Select(device =>
                device.GetProperty("name").GetString() + " — " + device.GetProperty("reportedDeviceType").GetString() +
                " " + device.GetProperty("number").GetUInt32() + "\nID: " + device.GetProperty("uniqueId").GetString() +
                (device.GetProperty("supportedDeviceType").ValueKind == JsonValueKind.Null ? "\nThis device class is not supported by Regain Hub." : "")));
            var index = 0;
            foreach (var device in catalog.GetProperty("devices").EnumerateArray()) selection.Items.Add(new ComboBoxItem {
                Content = device.GetProperty("name").GetString() + " — " + device.GetProperty("reportedDeviceType").GetString() + " " + device.GetProperty("number").GetUInt32(),
                Tag = index++, IsEnabled = device.GetProperty("supportedDeviceType").ValueKind != JsonValueKind.Null
            });
            status.Text = "Read " + catalog.GetProperty("devices").GetArrayLength() + " catalog entries. No equipment connection was opened; configuration is unchanged.";
        });
    }
    private void DiscoveryControls(bool editable) => discoveryPanel.IsEnabled = !busy && !closed && editable;
}
