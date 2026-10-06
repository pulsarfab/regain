using System.Text.Json;
using System.Windows;
using System.Windows.Controls;

namespace Regain.Hub;

public sealed partial class HubConfigurationWindow
{
    private readonly StackPanel credentials = new() { Margin = new Thickness(8) };
    private PasswordBox? authorization;
    private TextBox? credentialReference;
    private Button? createCredential, readCredential, deleteCredential;
    private string retainedCredentialReference = "";
    private void RenderCredentials()
    {
        authorization?.Clear(); credentials.Children.Clear();
        authorization = null; credentialReference = null;
        createCredential = readCredential = deleteCredential = null;
        if (session?.CredentialDescription is not JsonElement storage) {
            credentials.Children.Add(Text("This host has no credential storage provider.")); return;
        }
        var input = storage.GetProperty("input").GetProperty("authorization");
        var reference = storage.GetProperty("reference");
        credentials.Children.Add(Text("Upstream credentials", 22));
        credentials.Children.Add(Text(storage.GetProperty("protectionDescription").GetString() + ". " + storage.GetProperty("rotation").GetString()));
        credentials.Children.Add(Text(input.GetProperty("label").GetString()!));
        credentials.Children.Add(Text(input.GetProperty("description").GetString()!));
        authorization = new PasswordBox { MaxLength = input.GetProperty("maxLength").GetInt32(), Margin = new Thickness(4), ToolTip = input.GetProperty("description").GetString() };
        credentials.Children.Add(authorization);
        createCredential = CredentialButton("Save new credential");
        createCredential.Click += async (_, _) => await Run(async () => {
            var id = Guid.NewGuid();
            // Keep the reference visible before sending a write. Reload/Render
            // preserves it after a missing reply; creation is never repeated.
            retainedCredentialReference = session!.CredentialReference(id);
            credentialReference!.Text = retainedCredentialReference;
            var secret = authorization.Password; authorization.Clear();
            await session.CreateCredentialAsync(id, secret, lifetime.Token);
            preview.Clear();
            status.Text = "Credential saved. Copy its reference into the source settings, then review and apply the configuration.";
        });
        credentials.Children.Add(createCredential);
        credentials.Children.Add(Text(reference.GetProperty("label").GetString()!));
        credentials.Children.Add(Text(reference.GetProperty("description").GetString()!));
        credentialReference = new TextBox { Text = retainedCredentialReference, Margin = new Thickness(4), Tag = "credentialReference" };
        credentialReference.TextChanged += (_, _) => { retainedCredentialReference = credentialReference.Text; CredentialControls(session?.State is HubEditorState.Editing or HubEditorState.Reviewed); };
        credentials.Children.Add(credentialReference);
        var actions = new StackPanel { Orientation = Orientation.Horizontal }; credentials.Children.Add(actions);
        readCredential = CredentialButton("Read credential status"); deleteCredential = CredentialButton("Remove unused credential");
        actions.Children.Add(readCredential); actions.Children.Add(deleteCredential);
        readCredential.Click += async (_, _) => await Run(async () => {
            var result = await session!.CredentialStatusAsync(retainedCredentialReference, lifetime.Token);
            status.Text = result.GetProperty("present").GetBoolean() ? "Credential is present. Its value is never returned." : "Credential is absent. Check the reference before creating a new credential.";
        });
        deleteCredential.Click += async (_, _) => await Run(async () => {
            var result = await session!.DeleteCredentialAsync(retainedCredentialReference, lifetime.Token);
            preview.Clear();
            status.Text = (result.GetProperty("removed").GetBoolean() ? "Unused credential removed." : "Credential was already absent.") +
                (result.TryGetProperty("persistenceWarning", out var warning) ? " " + warning.GetString() : "");
        });
        credentials.Children.Add(Text("Removing a credential is refused while the saved configuration uses it. After a lost reply, reload and read the retained reference's status before another change."));
    }
    private void CredentialControls(bool editable)
    {
        var available = !busy && !closed && session?.State is HubEditorState.Editing or HubEditorState.Reviewed or HubEditorState.Blocked;
        if (authorization is not null) authorization.IsEnabled = available && editable;
        if (credentialReference is not null) credentialReference.IsEnabled = available;
        if (createCredential is not null) createCredential.IsEnabled = available && editable && session!.CredentialDescription!.Value.TryGetProperty("clientChosenReferences", out var chosen) && chosen.ValueKind == JsonValueKind.True;
        if (readCredential is not null) readCredential.IsEnabled = available && !string.IsNullOrWhiteSpace(retainedCredentialReference);
        if (deleteCredential is not null) deleteCredential.IsEnabled = available && editable && !string.IsNullOrWhiteSpace(retainedCredentialReference);
    }
    private static TextBlock Text(string text, double size = 14) => new() { Text = text, FontSize = size, TextWrapping = TextWrapping.Wrap, Margin = new Thickness(4, 6, 4, 6) };
    private static Button CredentialButton(string text) => new() { Content = text, Margin = new Thickness(4), Padding = new Thickness(10, 6, 10, 6), HorizontalAlignment = HorizontalAlignment.Left };
}
