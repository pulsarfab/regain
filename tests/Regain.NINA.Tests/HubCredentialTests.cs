using System.Text.Json;
using System.Text.Json.Nodes;
using System.Windows;
using System.Windows.Controls;
using Regain.Hub;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    [Fact]
    public async Task NativeCredentialsStoreOnlyReferencesAndInvalidateConfigurationReview()
    {
        await using var host = await Host.Open(); using var editor = await Editor(host); await editor.ReloadAsync();
        Assert.Equal("windowsDpapiUser", editor.CredentialDescription!.Value.GetProperty("protection").GetString());
        var id = Guid.NewGuid(); var reference = editor.CredentialReference(id);
        try {
            Assert.True(await editor.ReviewAsync());
            var created = await editor.CreateCredentialAsync(id, "Bearer native-private-fixture");
            Assert.True(created.GetProperty("present").GetBoolean());
            Assert.DoesNotContain("native-private-fixture", created.GetRawText());
            Assert.Equal(HubEditorState.Editing, editor.State);
            await Assert.ThrowsAsync<InvalidOperationException>(() => editor.ApplyAsync());
            Assert.True((await editor.CredentialStatusAsync(reference)).GetProperty("present").GetBoolean());
            var unchanged = await host.Command(new { op = "getConfig" });
            Assert.Equal(editor.Draft!.Revision, unchanged.GetProperty("revision").GetGuid());
            Assert.DoesNotContain(reference, unchanged.GetRawText());
            var diagnostics = editor.DiagnosticSnapshot().GetRawText();
            Assert.DoesNotContain("native-private-fixture", diagnostics); Assert.DoesNotContain(reference, diagnostics);
            for (int i = 0; i < 3; i++) Assert.Equal(0, (await host.Status(i)).GetProperty("leaseCount").GetInt32());
            Assert.True((await editor.DeleteCredentialAsync(reference)).GetProperty("removed").GetBoolean());
            Assert.False((await editor.CredentialStatusAsync(reference)).GetProperty("present").GetBoolean());
        } finally { await host.Command(new { op = "deleteCredential", reference }); }
    }
    [Theory]
    [InlineData(0)]
    [InlineData(1)]
    [InlineData(2)]
    public async Task NativeCredentialLostOrMalformedReplyCanBeCheckedWithoutRepeatingCreation(int failure)
    {
        var saved = HubDraftTests.Configuration(); var description = JsonNode.Parse(HubDraftTests.Description().GetRawText())!;
        description["credentialStorage"] = JsonSerializer.SerializeToNode(new {
            protection = "windowsDpapiUser", clientChosenReferences = true, referencePrefix = "credential-",
            reference = new { maxLength = 200 }, input = new { authorization = new { maxLength = 8192, writeOnly = true, sensitive = true } }
        });
        string? stored = null; int writes = 0;
        using var editor = new HubEditorSession(saved.GetProperty("instanceId").GetGuid(), (command, _) => {
            var op = command.GetProperty("op").GetString(); object result;
            switch (op) {
                case "describeConfig": result = description; break;
                case "getConfig": result = saved; break;
                case "hostStatus": result = new { phase = "ready", configurationRevision = saved.GetProperty("revision").GetGuid() }; break;
                case "createCredential":
                    writes++; stored = "credential-" + command.GetProperty("referenceId").GetGuid().ToString("D");
                    if (failure == 0) throw new HubException(HubFailure.Uncertain);
                    if (failure == 2) throw new HubException(HubFailure.Remote, new HubRemoteError(JsonSerializer.SerializeToElement(new {
                        code = "unavailable", message = "Protected credential storage is unavailable or invalid"
                    })));
                    result = new { reference = stored, present = true, protection = "windowsDpapiUser", authorization = "must never be returned" }; break;
                case "credentialStatus": result = new { reference = stored, present = true, protection = "windowsDpapiUser" }; break;
                default: throw new InvalidOperationException("Unexpected credential operation");
            }
            return Task.FromResult(JsonSerializer.SerializeToElement(result));
        }, () => { });
        await editor.ReloadAsync(); var id = Guid.NewGuid(); var reference = editor.CredentialReference(id);
        var error = await Assert.ThrowsAsync<HubException>(() => editor.CreateCredentialAsync(id, "Bearer lost-reply-fixture"));
        Assert.Equal(failure == 1 ? HubFailure.Protocol : failure == 2 ? HubFailure.Remote : HubFailure.Uncertain, error.Failure);
        Assert.DoesNotContain("must never be returned", error.Message);
        Assert.Equal(HubEditorState.Uncertain, editor.State);
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.CreateCredentialAsync(id, "Bearer another-value"));
        Assert.Equal(1, writes); await editor.ReloadAsync();
        Assert.True((await editor.CredentialStatusAsync(reference)).GetProperty("present").GetBoolean());
        Assert.Equal(1, writes); Assert.Equal(reference, stored);
    }
    [Fact]
    public async Task NativeCredentialWindowClearsSecretAndRetainsReferenceAcrossReload()
    {
        await Wpf(async () => {
            await using var host = await Host.Open();
            var window = new HubConfigurationWindow(host.Executable, host.ConfigPath, host.Selection(0, "switch").InstanceId);
            string? reference = null;
            try {
                window.Show(); var tabs = Controls<TabControl>(window).Single();
                await UiUntil(() => Controls<Button>(window).Any(b => (string)b.Content == "Save new credential" && b.IsEnabled));
                tabs.SelectedIndex = 3;
                var password = Controls<PasswordBox>(window).Single(); password.Password = "Bearer private-window-fixture";
                Controls<Button>(window).Single(b => (string)b.Content == "Save new credential").RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                var read = Controls<Button>(window).Single(b => (string)b.Content == "Read credential status");
                await UiUntil(() => read.IsEnabled);
                reference = Controls<TextBox>(window).Single(b => (string?)b.Tag == "credentialReference").Text;
                Assert.StartsWith("credential-", reference); Assert.Empty(password.Password);
                Assert.True((await host.Command(new { op = "credentialStatus", reference })).GetProperty("present").GetBoolean());
                Controls<Button>(window).Single(b => (string)b.Content == "Reload saved configuration").RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                await UiUntil(() => Controls<Button>(window).Any(b => (string)b.Content == "Read credential status" && b.IsEnabled));
                Assert.Equal(reference, Controls<TextBox>(window).Single(b => (string?)b.Tag == "credentialReference").Text);
                await Capture(window, "hub-native-credentials-simulation.png");
                var remove = Controls<Button>(window).Single(b => (string)b.Content == "Remove unused credential");
                remove.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(() => remove.IsEnabled);
                Assert.False((await host.Command(new { op = "credentialStatus", reference })).GetProperty("present").GetBoolean());
                for (int i = 0; i < 3; i++) Assert.Equal(0, (await host.Status(i)).GetProperty("leaseCount").GetInt32());
            } finally { window.Close(); if (reference is not null) await host.Command(new { op = "deleteCredential", reference }); }
        });
    }
}
