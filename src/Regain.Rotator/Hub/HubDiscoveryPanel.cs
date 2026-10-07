using System.Text.Json;
using System.Windows;
using System.Windows.Controls;

namespace Regain.Hub;

public sealed partial class HubConfigurationWindow
{
    private readonly StackPanel discoveryPanel = new() { Margin = new Thickness(8) };
    private TextBox? discoveryUrl, discoveryCredential;
    private void RenderDiscovery()
    {
        discoveryPanel.Children.Clear();
        var description = session!.DiscoveryDescription;
        var parameters = description.GetProperty("parameters");
        discoveryPanel.Children.Add(Text("Find devices on an Alpaca server", 22));
        discoveryPanel.Children.Add(Text("Read the server's device catalog without connecting equipment. Results do not change your sources or draft. Use supported entries when configuring an Alpaca source."));
        var url = parameters.GetProperty("baseUrl");
        discoveryPanel.Children.Add(Text(url.GetProperty("label").GetString()!));
        discoveryUrl = new TextBox { Margin = new Thickness(4), MaxLength = url.GetProperty("maxLength").GetInt32(), Tag = "discovery-url" };
        discoveryPanel.Children.Add(discoveryUrl);
        var credential = parameters.GetProperty("credentialReference");
        discoveryPanel.Children.Add(Text(credential.GetProperty("label").GetString()!));
        discoveryPanel.Children.Add(Text(credential.GetProperty("description").GetString()!));
        discoveryCredential = new TextBox { Margin = new Thickness(4), Tag = "discovery-credential" }; discoveryPanel.Children.Add(discoveryCredential);
        discoveryPanel.Children.Add(Text($"One query, up to {description.GetProperty("timeoutSeconds").GetInt32()} seconds and {description.GetProperty("maximumDevices").GetInt32()} devices. Enter the server URL, including any reverse-proxy prefix."));
        var query = CredentialButton("Read Alpaca device catalog"); discoveryPanel.Children.Add(query);
        var result = new TextBox { IsReadOnly = true, TextWrapping = TextWrapping.Wrap, AcceptsReturn = true,
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto, MinHeight = 180, Tag = "discovery-result", Margin = new Thickness(4) };
        discoveryPanel.Children.Add(result);
        query.Click += async (_, _) => await Run(async () => {
            result.Clear();
            var catalog = await session!.DiscoverAlpacaAsync(discoveryUrl.Text,
                string.IsNullOrWhiteSpace(discoveryCredential.Text) ? null : discoveryCredential.Text, lifetime.Token);
            result.Text = string.Join("\n\n", catalog.GetProperty("devices").EnumerateArray().Select(device =>
                device.GetProperty("name").GetString() + " — " + device.GetProperty("reportedDeviceType").GetString() +
                " " + device.GetProperty("number").GetUInt32() + "\nID: " + device.GetProperty("uniqueId").GetString() +
                (device.GetProperty("supportedDeviceType").ValueKind == JsonValueKind.Null ? "\nThis device class is not supported by Regain Hub." : "")));
            status.Text = "Read " + catalog.GetProperty("devices").GetArrayLength() + " catalog entries. No equipment connection was opened; configuration is unchanged.";
        });
    }
    private void DiscoveryControls(bool editable) => discoveryPanel.IsEnabled = !busy && !closed && editable;
}
