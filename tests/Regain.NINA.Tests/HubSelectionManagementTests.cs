using Regain.Hub;
using NINA.Equipment.Interfaces;
using System.Windows;
using System.Windows.Controls;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    [Fact]
    public async Task SavedChoiceManagerReconcilesConflictsBeforeRemovingTheSelectedEntry()
    {
        await Wpf(async () => {
            var directory = Path.Combine(Path.GetTempPath(), "Regain choice manager " + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(directory);
            var store = new HubSelectionStore(Path.Combine(directory, "bindings.json"));
            var first = Binding(directory); first.Label = "Automated selection simulation"; first.Simulated = true;
            var second = Binding(directory, "safetymonitor");
            var saved = store.Save(first, Guid.Empty);
            var window = new HubSelectionManagerWindow(store, "switch");
            try {
                window.Show();
                var list = Controls<ListBox>(window).Single();
                var remove = Controls<Button>(window).Single(b => (string)b.Content == "Remove selected saved choice");
                var reload = Controls<Button>(window).Single(b => (string)b.Content == "Reload saved choices");
                Assert.Single(list.Items.Cast<object>()); Assert.False(remove.IsEnabled);
                list.SelectedIndex = 0; Assert.True(remove.IsEnabled);
                await Capture(window, "hub-native-selections-simulation.png");
                store.Save(second, saved.Revision); // Another frontend saved meanwhile.
                remove.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                Assert.Equal(2, store.Load().Bindings.Length); Assert.False(remove.IsEnabled);
                Assert.Contains(Controls<TextBlock>(window), t => t.Text.Contains("Reload to reconcile"));
                reload.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                list.SelectedIndex = 0; remove.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                Assert.Equal(second.Id, Assert.Single(store.Load().Bindings).Id);
                Assert.Empty(list.Items.Cast<object>()); Assert.False(remove.IsEnabled);
            } finally { window.Close(); Directory.Delete(directory, true); }
        });
    }
    [Fact]
    public void SelectionRemovalPreservesOtherChoicesAndRejectsConflictsAndMissingIdentities()
    {
        var directory = Path.Combine(Path.GetTempPath(), "Regain removal " + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try {
            var store = new HubSelectionStore(Path.Combine(directory, "bindings.json"));
            var first = Binding(directory); var second = Binding(directory, "safetymonitor");
            var saved = store.Save(first, Guid.Empty);
            var changed = store.Save(second, saved.Revision);
            var before = File.ReadAllBytes(store.Path);
            Assert.Throws<InvalidOperationException>(() => store.Remove(first.InstanceId, first.OutputId, saved.Revision));
            Assert.Equal(before, File.ReadAllBytes(store.Path));
            Assert.Throws<ArgumentException>(() => store.Remove(Guid.Empty, first.OutputId, changed.Revision));
            Assert.Throws<InvalidOperationException>(() => store.Remove(first.InstanceId, Guid.NewGuid(), changed.Revision));
            Assert.Equal(before, File.ReadAllBytes(store.Path));
            var removed = store.Remove(first.InstanceId, first.OutputId, changed.Revision);
            Assert.NotEqual(changed.Revision, removed.Revision);
            Assert.Equal(second.Id, Assert.Single(store.Load().Bindings).Id);
            // Empty is a valid durable chooser list, retaining a revision for CAS.
            var empty = store.Remove(second.InstanceId, second.OutputId, removed.Revision);
            Assert.Empty(store.Load().Bindings); Assert.NotEqual(Guid.Empty, empty.Revision);
            var restored = store.Save(first, empty.Revision);
            Assert.Equal(first.Id, Assert.Single(restored.Bindings).Id);
        } finally { Directory.Delete(directory, true); }
    }
    [Fact]
    public void SelectionRemovalNeverOverwritesAnUnreadableFile()
    {
        var path = Path.Combine(Path.GetTempPath(), "Regain unreadable removal " + Guid.NewGuid().ToString("N") + ".json");
        try {
            File.WriteAllText(path, "{\"schemaVersion\":2}");
            var before = File.ReadAllBytes(path);
            Assert.Throws<InvalidOperationException>(() => new HubSelectionStore(path).Remove(Guid.NewGuid(), Guid.NewGuid(), Guid.Empty));
            Assert.Equal(before, File.ReadAllBytes(path));
        } finally { File.Delete(path); File.Delete(path + ".lock"); }
    }
    [Fact]
    public async Task RemovingNativeChoiceDoesNotRevokeAConnectedOutputOrChangeItsIdentity()
    {
        await using var host = await Host.Open();
        var store = new HubSelectionStore(Path.Combine(host.DirectoryPath, "bindings.json"));
        var binding = host.Selection(0, "switch");
        var saved = store.Save(binding, Guid.Empty);
        using var device = host.Switch();
        await device.Connect(CancellationToken.None);
        await Eventually(async () => (await host.Status(0)).GetProperty("leaseCount").GetInt32() == 1);
        var removed = store.Remove(binding.InstanceId, binding.OutputId, saved.Revision);
        Assert.True(device.Connected); Assert.Equal(binding.Id, device.Id);
        Assert.Equal(1, (await host.Status(0)).GetProperty("leaseCount").GetInt32());
        var choices = HubEquipment.Choices<ISwitchHub>("switch", b => new HubSwitchDevice(b), store);
        Assert.EndsWith("Configure.switch", Assert.Single(choices).Id);
        foreach (var choice in choices) ((IDisposable)choice).Dispose();
        var restored = store.Save(binding, removed.Revision);
        Assert.Equal(binding.Id, Assert.Single(restored.Bindings).Id);
        Assert.True(device.Connected);
        device.Disconnect();
        await Eventually(async () => (await host.Status(0)).GetProperty("leaseCount").GetInt32() == 0);
    }
}
