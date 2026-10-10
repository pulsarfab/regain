using System.Windows;
using System.Windows.Controls;
using Regain.Hub.ASCOM;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    [Fact]
    public async Task AscomManagerUsesSharedThemeAndReconcilesOrphanRemovalWithoutEquipment()
    {
        await Wpf(async () => {
            using var f = new global::HubRegistrationTests.Fixture();
            var operations = new List<HubRegistrationRequest>();
            var window = new HubAscomManagerWindow(f.Directory, f.Store,
                () => f.Roots.SelectMany(root => HubAscomRegistration.RegisteredOutputs(root)).ToArray(),
                request => {
                    operations.Add(request);
                    if (request.Arguments.StartsWith("\"/hubregister\"", StringComparison.Ordinal)) f.Register();
                    else f.Remove();
                    return Task.FromResult(0);
                });
            try {
                window.Show();
                var list = Controls<ListBox>(window).Single();
                var register = Controls<Button>(window).Single(b => (string)b.Content == "Register / refresh in ASCOM");
                var remove = Controls<Button>(window).Single(b => (string)b.Content == "Remove ASCOM registration");
                var forget = Controls<Button>(window).Single(b => (string)b.Content == "Remove saved choice");
                var reload = Controls<Button>(window).Single(b => (string)b.Content == "Reload inventory");
                Assert.Empty(operations); Assert.NotNull(window.Icon);
                list.SelectedIndex = 0;
                Assert.True(register.IsEnabled); Assert.False(remove.IsEnabled); Assert.True(forget.IsEnabled);
                register.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                await UiUntil(() => operations.Count == 1 && reload.IsEnabled);
                Assert.Equal(f.Saved.Revision, f.Store.Load().Revision);
                Assert.Equal(OutputIdentity.ClassId(f.Binding), operations[0].ClassId);
                Assert.Contains("\"" + f.Store.Path + "\"", operations[0].Arguments);
                Assert.Contains(f.Saved.Revision.ToString("D"), operations[0].Arguments);
                list.SelectedIndex = 0;
                Assert.True(remove.IsEnabled); Assert.False(forget.IsEnabled);
                await Capture(window, "hub-ascom-registration-simulation.png");
                File.Delete(f.Store.Path); // A missing file must not hide owned registration.
                reload.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                list.SelectedIndex = 0;
                Assert.False(register.IsEnabled); Assert.True(remove.IsEnabled);
                Assert.Contains(Controls<TextBlock>(window), t => t.Text.Contains("saved choice missing"));
                remove.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                await UiUntil(() => operations.Count == 2 && reload.IsEnabled);
                Assert.Empty(list.Items.Cast<object>());
                Assert.All(f.Roots, root => Assert.Empty(HubAscomRegistration.RegisteredIds(root)));
            } finally { window.Close(); }
        });
    }

    [Fact]
    public async Task AscomManagerRequiresExplicitReloadAfterUnknownOrFailedRegistration()
    {
        await Wpf(async () => {
            using var f = new global::HubRegistrationTests.Fixture();
            var completion = new TaskCompletionSource<int>(); var calls = 0;
            var window = new HubAscomManagerWindow(f.Directory, f.Store, () => [], request => { calls++; return completion.Task; });
            try {
                window.Show();
                var list = Controls<ListBox>(window).Single();
                var register = Controls<Button>(window).Single(b => (string)b.Content == "Register / refresh in ASCOM");
                var reload = Controls<Button>(window).Single(b => (string)b.Content == "Reload inventory");
                list.SelectedIndex = 0; register.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                Assert.Equal(1, calls); Assert.False(reload.IsEnabled); Assert.False(list.IsEnabled);
                completion.SetException(new TimeoutException());
                await UiUntil(() => reload.IsEnabled);
                Assert.False(register.IsEnabled);
                Assert.Contains(Controls<TextBlock>(window), t => t.Text.Contains("unknown completion"));
                register.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); Assert.Equal(1, calls);
                reload.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); list.SelectedIndex = 0;
                Assert.True(register.IsEnabled); Assert.Equal(1, calls);
            } finally { window.Close(); }
        });
    }

    [Fact]
    public async Task AscomManagerProtectsOtherOwnersAndUnreadableSelections()
    {
        await Wpf(async () => {
            using var f = new global::HubRegistrationTests.Fixture();
            var record = new HubRegisteredOutput(OutputIdentity.ClassId(f.Binding), f.Binding, f.Directory,
                f.Store.Path, "S-1-5-18", new Version(0, 1), "ready");
            var window = new HubAscomManagerWindow(f.Directory, f.Store, () => [record], _ => throw new Exception("Must not elevate"));
            try {
                window.Show(); var list = Controls<ListBox>(window).Single(); list.SelectedIndex = 0;
                Assert.False(Controls<Button>(window).Single(b => (string)b.Content == "Register / refresh in ASCOM").IsEnabled);
                Assert.False(Controls<Button>(window).Single(b => (string)b.Content == "Remove ASCOM registration").IsEnabled);
                File.WriteAllText(f.Store.Path, "invalid");
                Controls<Button>(window).Single(b => (string)b.Content == "Reload inventory").RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                Assert.Empty(list.Items.Cast<object>());
                Assert.All(Controls<Button>(window).Where(b => (string)b.Content != "Reload inventory"), b => Assert.False(b.IsEnabled));
            } finally { window.Close(); }
            await Task.CompletedTask;
        });
    }
}
