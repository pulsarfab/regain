using System.Text.Json;
using NINA.Equipment.Interfaces;
using Regain.Hub;
using Regain.TestFixtures;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    [Fact]
    public async Task NativeFocuserWaitsForCompletionAndPreservesSiblingLeases()
    {
        using var server = new HubFocuserServer();
        await using var host = await Host.Open(config => server.AddTo(config, 4, 7));
        using var first = new HubFocuserDevice(host.Selection(3, "focuser"), host.Executable, host.Workers);
        using var sibling = new HubFocuserDevice(host.Selection(4, "focuser"), host.Executable, host.Workers);
        Assert.False(first.Connected); Assert.Equal(0, server.Moves);
        Assert.True(await first.Connect(CancellationToken.None)); Assert.True(await sibling.Connect(CancellationToken.None));
        Assert.Equal(50, first.Position); Assert.Equal(1000, first.MaxStep); Assert.Equal(100, first.MaxIncrement);
        Assert.True(first.TempCompAvailable); Assert.True(first.TempComp); Assert.Equal(-5, first.Temperature);
        Assert.True(double.IsNaN(first.StepSize));
        Assert.Equal("", first.LastError);
        first.TempComp = false; Assert.False(sibling.TempComp);
        var moving = first.Move(70, CancellationToken.None, 0);
        await Eventually(() => Task.FromResult(server.Moves == 1));
        Assert.False(moving.IsCompleted); Assert.True(sibling.IsMoving); Assert.Equal(70, sibling.Position);
        server.Values["ismoving"] = false; await moving;
        Assert.Equal(0, server.Halts);
        await Assert.ThrowsAsync<ArgumentOutOfRangeException>(() => first.Move(71, CancellationToken.None, -1));
        using (var cancelled = new CancellationTokenSource()) {
            cancelled.Cancel(); await Assert.ThrowsAnyAsync<OperationCanceledException>(() => first.Move(71, cancelled.Token, 0));
        }
        Assert.Equal(1, server.Moves);
        first.Disconnect(); Assert.False(first.Connected); Assert.True(sibling.Connected);
        Assert.Equal(70, sibling.Position); Assert.Equal(0, server.Halts);
        sibling.Halt(); Assert.Equal(1, server.Halts);
        sibling.Disconnect();
        await Eventually(async () => (await host.Command(new { op = "sourceStatus", source = server.SourceId })).GetProperty("leaseCount").GetInt32() == 0);
    }
    [Fact]
    public async Task NativeFocuserCancellationAndStoppedShortDoNotIssueAnAutomaticHalt()
    {
        using var server = new HubFocuserServer();
        await using var host = await Host.Open(config => server.AddTo(config, 4));
        using var focuser = new HubFocuserDevice(host.Selection(3, "focuser"), host.Executable, host.Workers);
        await focuser.Connect(CancellationToken.None);
        using var cancellation = new CancellationTokenSource();
        var moving = focuser.Move(80, cancellation.Token, 0);
        await Eventually(() => Task.FromResult(server.Moves == 1));
        cancellation.Cancel(); await Assert.ThrowsAnyAsync<OperationCanceledException>(() => moving);
        Assert.Equal(1, server.Moves); Assert.Equal(0, server.Halts);
        focuser.Disconnect(); server.Values["ismoving"] = false;
        await focuser.Connect(CancellationToken.None);
        var shortMove = focuser.Move(90, CancellationToken.None, 0);
        await Eventually(() => Task.FromResult(server.Moves == 2));
        server.Values["position"] = 85; server.Values["ismoving"] = false;
        await Assert.ThrowsAsync<IOException>(() => shortMove);
        Assert.Equal(0, server.Halts);
    }
    [Fact]
    public async Task NativeFocuserLostMoveReplyRetainsSharedUncertaintyAndNoReplay()
    {
        using var server = new HubFocuserServer { LoseMoveReply = true };
        await using var host = await Host.Open(config => server.AddTo(config, 4, 7));
        using var first = new HubFocuserDevice(host.Selection(3, "focuser"), host.Executable, host.Workers);
        using var second = new HubFocuserDevice(host.Selection(4, "focuser"), host.Executable, host.Workers);
        async Task ConnectWithEvidence(HubFocuserDevice device, string stage)
        {
            try { Assert.True(await device.Connect(CancellationToken.None)); }
            catch (HubException error) {
                // Keep the original failure and deadlines. CI must distinguish
                // a cold transport/capability failure from the injected Move.
                var state = await host.Command(new { op = "sourceStatus", source = server.SourceId });
                throw new Xunit.Sdk.XunitException($"{stage}: {error.Failure}, code={error.Remote?.Code}, message={error.Remote?.Message}, " +
                    $"source={state.GetRawText()}, private request trace={server.RequestTrace}");
            }
        }
        await ConnectWithEvidence(first, "first output initial connection");
        await ConnectWithEvidence(second, "second output initial connection");
        var lost = await Assert.ThrowsAsync<HubException>(() => first.Move(70, CancellationToken.None, 0));
        Assert.Equal("uncertain", lost.Remote!.Code);
        var refused = await Assert.ThrowsAsync<HubException>(() => second.Move(71, CancellationToken.None, 0));
        Assert.Equal("uncertain", refused.Remote!.Code);
        Assert.False(first.Connected); Assert.False(second.Connected);
        Assert.Equal(1, server.Moves); Assert.Equal(0, server.Halts);
        Assert.True((await host.Command(new { op = "sourceStatus", source = server.SourceId })).GetProperty("writeUncertain").GetBoolean());
    }
    [Fact]
    public async Task NativeFocuserRejectsRelativeSourcesWithoutInventingPosition()
    {
        using var server = new HubFocuserServer(); server.Values["absolute"] = false;
        await using var host = await Host.Open(config => server.AddTo(config, 4));
        using var focuser = new HubFocuserDevice(host.Selection(3, "focuser"), host.Executable, host.Workers);
        await Assert.ThrowsAsync<NotSupportedException>(() => focuser.Connect(CancellationToken.None));
        Assert.False(focuser.Connected); Assert.Equal(0, server.Moves);
        await Eventually(async () => (await host.Command(new { op = "sourceStatus", source = server.SourceId })).GetProperty("leaseCount").GetInt32() == 0);
    }
    [Fact]
    public async Task NativeFocuserInvalidMotionReadingIsAnErrorNotAnIdleState()
    {
        using var server = new HubFocuserServer();
        await using var host = await Host.Open(config => server.AddTo(config, 4));
        using var focuser = new HubFocuserDevice(host.Selection(3, "focuser"), host.Executable, host.Workers);
        await focuser.Connect(CancellationToken.None);
        server.Values["ismoving"] = "false";
        var read = Assert.Throws<HubException>(() => _ = focuser.IsMoving);
        Assert.Equal("unavailable", read.Remote!.Code);
        var move = await Assert.ThrowsAsync<HubException>(() => focuser.Move(70, CancellationToken.None, 0));
        Assert.Equal("unavailable", move.Remote!.Code);
        Assert.Equal(0, server.Moves); Assert.True(focuser.Connected);
        Assert.Equal(50, focuser.Position);
    }
    [Fact]
    public void NativeFocuserChoicesNeedNoHostAndTypedReadingsRejectInvalidValues()
    {
        var directory = Path.Combine(Path.GetTempPath(), "Regain focuser choices " + Guid.NewGuid().ToString("N")); Directory.CreateDirectory(directory);
        try {
            var store = new HubSelectionStore(Path.Combine(directory, "bindings.json"));
            var binding = Binding(directory, "focuser"); store.Save(binding, Guid.Empty);
            var choices = HubEquipment.Choices<IFocuser>("focuser", selection => new HubFocuserDevice(selection), store);
            Assert.Equal(binding.Id, choices[0].Id); Assert.EndsWith("Configure.focuser", choices[1].Id);
            foreach (var choice in choices) ((IDisposable)choice).Dispose();
            foreach (var (property, json) in new[] { (HubFocuserProperty.IsMoving, "\"false\""), (HubFocuserProperty.Position, "-1"),
                (HubFocuserProperty.MaxStep, "0"), (HubFocuserProperty.MaxIncrement, "1.5"), (HubFocuserProperty.Temperature, "\"NaN\""), (HubFocuserProperty.StepSize, "0") }) {
                using var document = JsonDocument.Parse(json);
                Assert.Throws<HubException>(() => HubFocuserProtocol.Validate(property, document.RootElement));
            }
        } finally { Directory.Delete(directory, true); }
    }
}
