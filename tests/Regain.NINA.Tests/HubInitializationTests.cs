using System.Text.Json;
using System.Text.Json.Nodes;
using System.Diagnostics;
using System.Windows;
using System.Windows.Controls;
using Regain.Hub;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    [Fact]
    public async Task NativeCreationUsesProductionPersistenceAndPreservesExistingData()
    {
        await using var host = await Host.Open();
        var path = Path.Combine(host.DirectoryPath, "new configuration λ.json");
        var creation = new HubInitialization(host.Executable);
        await creation.CreateAsync(path);
        Assert.Equal(HubInitializationState.Created, creation.State); Assert.Equal(path, creation.Path);
        var configuration = JsonNode.Parse(await File.ReadAllTextAsync(path))!;
        Assert.Empty(configuration["sources"]!.AsArray()); Assert.Empty(configuration["outputs"]!.AsArray());
        Assert.Equal(Guid.Parse(configuration["instanceId"]!.GetValue<string>()),creation.InstanceId);
        var bytes = await File.ReadAllBytesAsync(path);
        await Assert.ThrowsAsync<HubException>(() => creation.CreateAsync(path));
        Assert.True(creation.RequiresReconciliation); Assert.Equal(bytes,await File.ReadAllBytesAsync(path));
        await creation.ReadAsync(); Assert.Equal(HubInitializationState.Existing,creation.State);
        Assert.Equal(path,creation.Path);
        Assert.Equal(0,(await host.Status(0)).GetProperty("leaseCount").GetInt32());
        await File.WriteAllTextAsync(path,"preserve invalid file");
        await Assert.ThrowsAnyAsync<Exception>(() => creation.ReadAsync()); Assert.True(creation.RequiresReconciliation);
        await Assert.ThrowsAsync<InvalidOperationException>(() => creation.CreateAsync(Path.Combine(host.DirectoryPath,"different.json")));
        Assert.Equal("preserve invalid file",await File.ReadAllTextAsync(path));
    }
    [Theory]
    [InlineData("lost")]
    [InlineData("malformed")]
    [InlineData("cancelled")]
    public async Task UnknownCreationRetainsFilenameAndReadsCommittedFileWithoutReplay(string failure)
    {
        await using var host = await Host.Open(); var path = Path.Combine(host.DirectoryPath,"unknown.json");
        using var cancellation = new CancellationTokenSource(); int writes = 0;
        var creation = new HubInitialization(async (chosen,token) => {
            writes++; var result = await HubAttachment.RunHelperAsync(host.Executable,"--hub-init --hub-config " + HubAttachment.Quote(chosen),token);
            if (failure=="malformed") { Array.Clear(result); return "{}"u8.ToArray(); }
            if (failure=="cancelled") { cancellation.Cancel(); return result; }
            Array.Clear(result); throw new HubException(HubFailure.Disconnected);
        });
        await Assert.ThrowsAnyAsync<Exception>(() => creation.CreateAsync(path,cancellation.Token));
        Assert.True(creation.RequiresReconciliation); Assert.Equal(path,creation.Path); Assert.Null(creation.InstanceId);
        await Assert.ThrowsAsync<InvalidOperationException>(() => creation.CreateAsync(path)); Assert.Equal(1,writes);
        await creation.ReadAsync(); Assert.Equal(HubInitializationState.Existing,creation.State); Assert.Equal(1,writes);
        Assert.Equal(0,(await host.Status(0)).GetProperty("leaseCount").GetInt32());
    }
    [Fact]
    public async Task PendingCreationBlocksOtherOperationsAndMissingFileReconcilesExplicitly()
    {
        await using var host = await Host.Open(); var path = Path.Combine(host.DirectoryPath,"never-published.json");
        int writes=0; var release = new TaskCompletionSource<byte[]>(TaskCreationOptions.RunContinuationsAsynchronously);
        var creation = new HubInitialization((_,_) => { writes++; return release.Task; });
        await Assert.ThrowsAsync<HubException>(() => creation.CreateAsync("relative.json")); Assert.Equal(0,writes);
        var pending = creation.CreateAsync(path); Assert.Equal(HubInitializationState.Creating,creation.State);
        Assert.Equal(HubFailure.Busy,(await Assert.ThrowsAsync<HubException>(() => creation.ReadAsync())).Failure);
        Assert.Equal(HubFailure.Busy,(await Assert.ThrowsAsync<HubException>(() => creation.CreateAsync(path))).Failure);
        release.SetException(new HubException(HubFailure.Uncertain)); await Assert.ThrowsAsync<HubException>(() => pending);
        await creation.ReadAsync(); Assert.Equal(HubInitializationState.Missing,creation.State); Assert.False(creation.RequiresReconciliation);
        Assert.Equal(path,creation.Path); Assert.Equal(1,writes); Assert.False(File.Exists(path));
    }
    [Fact]
    public async Task SharedNativeSelectorRetainsUnknownFilenameAndEnablesLoadingOnlyAfterRead()
    {
        await Wpf(async () => {
            await using var host = await Host.Open(); var path = Path.Combine(host.DirectoryPath,"created-from-native.json");
            var creation = new HubInitialization(async (chosen, token) => {
                var reply = await HubAttachment.RunHelperAsync(host.Executable,"--hub-init --hub-config " + HubAttachment.Quote(chosen),token);
                Array.Clear(reply); throw new HubException(HubFailure.Uncertain);
            });
            var window = new HubSelectionWindow(host.Executable,new HubSelectionStore(Path.Combine(host.DirectoryPath,"selections.json")),"switch",null,creation) {
                ChooseNewConfigurationPath = () => path, Height = 900
            };
            Process? createdHost = null; DateTime started = default;
            try {
                window.Show(); var create = Controls<Button>(window).Single(b=>(string)b.Content=="Create new configuration…");
                var read = Controls<Button>(window).Single(b=>(string)b.Content=="Read retained configuration file");
                var load = Controls<Button>(window).Single(b=>(string)b.Content=="Load hub outputs");
                create.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(()=>read.IsEnabled);
                Assert.True(creation.RequiresReconciliation); Assert.False(create.IsEnabled); Assert.False(load.IsEnabled);
                Assert.Equal(path,Controls<TextBox>(window).Single().Text); Assert.True(Controls<TextBox>(window).Single().IsEnabled);
                Assert.True(Controls<TextBox>(window).Single().IsReadOnly); Assert.Equal(path,Controls<TextBox>(window).Single().ToolTip);
                await Capture(window,"hub-native-initialization.png");
                read.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(()=>load.IsEnabled);
                Assert.Equal(HubInitializationState.Existing,creation.State); Assert.True(create.IsEnabled);
                Controls<TextBox>(window).Single().Text = host.ConfigPath;
                read.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(()=>load.IsEnabled);
                Assert.Equal(path,Controls<TextBox>(window).Single().Text);
                Assert.False(Controls<Button>(window).Single(b=>(string)b.Content=="Edit shared configuration").IsEnabled);
                Assert.Equal(0,(await host.Status(0)).GetProperty("leaseCount").GetInt32());
                // Explicitly launch/record our own empty fixture so UI loading
                // can attach to it and cleanup never guesses a shared host PID.
                var attached = await HubAttachment.AttachAsync(host.Executable,path,host.Workers);
                Assert.NotNull(attached.StartedProcessId);
                createdHost = Process.GetProcessById(checked((int)attached.StartedProcessId!.Value)); started = createdHost.StartTime;
                load.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                await UiUntil(()=>Controls<Button>(window).Single(b=>(string)b.Content=="Edit shared configuration").IsEnabled);
                Assert.Empty(Controls<ComboBox>(window).Single().Items.Cast<object>());
                Assert.False(Controls<Button>(window).Single(b=>(string)b.Content=="Save selected output").IsEnabled);
                using var client = await HubClient.ConnectAsync(attached);
                Assert.Empty((await client.RequestAsync(JsonSerializer.SerializeToElement(new { op="listDevices" }))).EnumerateArray());
                var editor = new HubConfigurationWindow(host.Executable,path,attached.InstanceId);
                try {
                    editor.Show(); await UiUntil(()=>Controls<Button>(editor).Single(b=>(string)b.Content=="Review changes").IsEnabled);
                    Assert.Empty((await client.RequestAsync(JsonSerializer.SerializeToElement(new { op="getConfig" }))).GetProperty("sources").EnumerateArray());
                } finally { editor.Close(); }
            } finally {
                window.Close();
                if (createdHost is not null) {
                    using (createdHost) if (!createdHost.HasExited) {
                        Assert.Equal(Path.GetFullPath(host.Executable),createdHost.MainModule!.FileName,ignoreCase:true);
                        Assert.Equal(started,createdHost.StartTime); createdHost.Kill(); await createdHost.WaitForExitAsync();
                    }
                }
            }
        });
    }
}
