using System.Text.Json;
using NINA.Equipment.Interfaces;
using Regain.Hub;
using Regain.TestFixtures;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    [Fact]
    public async Task NativeRotatorUsesSharedSourceCoordinatesAndWaitsForEachAcceptedTarget()
    {
        using var source = new HubRotatorServer();
        await using var host = await Host.Open(config => source.AddTo(config, 4, 7));
        using var first = new HubRotatorDevice(host.Selection(3, "rotator"), host.Executable, host.Workers);
        using var sibling = new HubRotatorDevice(host.Selection(4, "rotator"), host.Executable, host.Workers);
        Assert.False(first.Connected); Assert.False(first.Synced); Assert.Equal(0, source.Moves);
        await first.Connect(CancellationToken.None); await sibling.Connect(CancellationToken.None);
        Assert.True(first.CanReverse); Assert.False(first.Reverse); Assert.Equal(0.02f, first.StepSize);
        Assert.Equal(20f, first.Position); Assert.Equal(350f, first.MechanicalPosition);
        first.Sync(42.5f);
        Assert.True(first.Synced); Assert.False(sibling.Synced);
        Assert.Equal(42.5f, sibling.Position); Assert.Equal(350f, sibling.MechanicalPosition);
        sibling.Reverse = true; Assert.True(first.Reverse);
        var relative = first.Move(-721.5f, CancellationToken.None);
        await Eventually(() => Task.FromResult(source.Moves == 1));
        Assert.False(relative.IsCompleted); Assert.True(sibling.IsMoving);
        source.Values["ismoving"] = false; Assert.True(await relative);
        Assert.Equal(41f, sibling.Position); Assert.Contains("write move position=-721.5", source.RequestTrace);
        var physical = sibling.MoveAbsoluteMechanical(12.25f, CancellationToken.None);
        await Eventually(() => Task.FromResult(source.Moves == 2));
        source.Values["ismoving"] = false; Assert.True(await physical);
        Assert.Equal(12.25f, first.MechanicalPosition);
        var absolute = first.MoveAbsolute(50f, CancellationToken.None);
        await Eventually(() => Task.FromResult(source.Moves == 3));
        source.Values["ismoving"] = false; Assert.True(await absolute);
        first.Disconnect(); Assert.False(first.Synced); Assert.True(sibling.Connected);
        Assert.Equal(50f, sibling.Position); Assert.Equal(0, source.Halts);
        sibling.Halt(); Assert.Equal(1, source.Halts);
        sibling.Disconnect();
        await Eventually(async () => (await host.Command(new { op = "sourceStatus", source = source.SourceId })).GetProperty("leaseCount").GetInt32() == 0);
    }
    [Fact]
    public async Task NativeRotatorCancellationAndStoppedShortCannotHaltSiblingMotion()
    {
        using var source = new HubRotatorServer();
        await using var host = await Host.Open(config => source.AddTo(config, 4, 7));
        using var first = new HubRotatorDevice(host.Selection(3, "rotator"), host.Executable, host.Workers);
        using var sibling = new HubRotatorDevice(host.Selection(4, "rotator"), host.Executable, host.Workers);
        await first.Connect(CancellationToken.None); await sibling.Connect(CancellationToken.None);
        using var cancellation = new CancellationTokenSource();
        var moving = first.MoveAbsolute(30f, cancellation.Token);
        await Eventually(() => Task.FromResult(source.Moves == 1));
        cancellation.Cancel(); await Assert.ThrowsAnyAsync<OperationCanceledException>(() => moving);
        Assert.True(sibling.Connected); Assert.True(sibling.IsMoving); Assert.Equal(0, source.Halts);
        source.Values["ismoving"] = false;
        first.Disconnect(); await first.Connect(CancellationToken.None);
        var shortMove = first.MoveAbsolute(40f, CancellationToken.None);
        await Eventually(() => Task.FromResult(source.Moves == 2));
        source.Values["position"] = 35.0; source.Values["ismoving"] = false;
        await Assert.ThrowsAsync<IOException>(() => shortMove);
        Assert.Equal(0, source.Halts);
        using var cancelled = new CancellationTokenSource(); cancelled.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => first.MoveAbsolute(41f, cancelled.Token));
        Assert.Equal(2, source.Moves);
    }
    [Fact]
    public async Task NativeRotatorLostMoveReplyRetainsUncertaintyWithoutReplay()
    {
        using var source = new HubRotatorServer { LoseMoveReply = true };
        await using var host = await Host.Open(config => source.AddTo(config, 4, 7));
        using var first = new HubRotatorDevice(host.Selection(3, "rotator"), host.Executable, host.Workers);
        using var sibling = new HubRotatorDevice(host.Selection(4, "rotator"), host.Executable, host.Workers);
        await first.Connect(CancellationToken.None); await sibling.Connect(CancellationToken.None);
        var lost = await Assert.ThrowsAsync<HubException>(() => first.Move(5f, CancellationToken.None));
        await AssertAccessoryFailure(host, source, lost, "uncertain", "first rotator output lost Move reply");
        var fenced = await Assert.ThrowsAsync<HubException>(() => sibling.MoveAbsolute(35f, CancellationToken.None));
        await AssertAccessoryFailure(host, source, fenced, "uncertain", "second rotator output fenced Move");
        Assert.False(first.Connected); Assert.False(sibling.Connected);
        Assert.Equal(1, source.Moves); Assert.Equal(0, source.Halts);
        Assert.True((await host.Command(new { op = "sourceStatus", source = source.SourceId })).GetProperty("writeUncertain").GetBoolean());
    }
    [Fact]
    public async Task NativeRotatorIgnoredRelativeCommandCannotReportSuccessfulCompletion()
    {
        using var source = new HubRotatorServer { IgnoreMove = true };
        await using var host = await Host.Open(config => source.AddTo(config, 4));
        using var rotator = new HubRotatorDevice(host.Selection(3, "rotator"), host.Executable, host.Workers);
        await rotator.Connect(CancellationToken.None);
        var failure = await Assert.ThrowsAsync<IOException>(() => rotator.Move(5f, CancellationToken.None));
        Assert.Contains("do not replay", failure.Message);
        Assert.NotEmpty(rotator.LastError);
        Assert.Equal(20f, rotator.Position); Assert.Equal(1, source.Moves); Assert.Equal(0, source.Halts);
    }
    [Fact]
    public async Task NativeRotatorOptionalErrorsAndInvalidMotionNeverInventIdleOrCompletion()
    {
        using var source = new HubRotatorServer(); source.Values.TryRemove("stepsize", out _);
        await using var host = await Host.Open(config => source.AddTo(config, 4));
        using var rotator = new HubRotatorDevice(host.Selection(3, "rotator"), host.Executable, host.Workers);
        await rotator.Connect(CancellationToken.None);
        Assert.True(float.IsNaN(rotator.StepSize)); Assert.Equal("", rotator.LastError);
        source.Values["ismoving"] = "false";
        Assert.Equal("unavailable", Assert.Throws<HubException>(() => _ = rotator.IsMoving).Remote!.Code);
        var move = await Assert.ThrowsAsync<HubException>(() => rotator.MoveAbsolute(30f, CancellationToken.None));
        Assert.Equal("unavailable", move.Remote!.Code);
        Assert.Equal(0, source.Moves); Assert.True(rotator.Connected); Assert.Equal(20f, rotator.Position);
        Assert.Throws<ArgumentOutOfRangeException>(() => rotator.Sync(360));
        await Assert.ThrowsAsync<ArgumentOutOfRangeException>(() => rotator.Move(float.NaN, CancellationToken.None));
    }
    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task NativeRotatorMissingRequiredReversalCannotBecomeConnected(bool missingReverse)
    {
        using var source = new HubRotatorServer();
        if (missingReverse) source.Values.TryRemove("reverse", out _); else source.Values["canreverse"] = false;
        await using var host = await Host.Open(config => source.AddTo(config, 4));
        using var rotator = new HubRotatorDevice(host.Selection(3, "rotator"), host.Executable, host.Workers);
        var failure = await Assert.ThrowsAsync<HubException>(() => rotator.Connect(CancellationToken.None));
        Assert.Equal("unavailable", failure.Remote!.Code); Assert.False(rotator.Connected);
        Assert.Equal(0, source.Moves); Assert.Equal(0, source.Halts);
        await Eventually(async () => (await host.Command(new { op = "sourceStatus", source = source.SourceId })).GetProperty("leaseCount").GetInt32() == 0);
    }
    [Fact]
    public void NativeRotatorChoicesAndStrictTypedContractsNeedNoHost()
    {
        var directory = Path.Combine(Path.GetTempPath(), "Regain rotator choices " + Guid.NewGuid().ToString("N")); Directory.CreateDirectory(directory);
        try {
            var store = new HubSelectionStore(Path.Combine(directory, "bindings.json"));
            var binding = Binding(directory, "rotator"); store.Save(binding, Guid.Empty);
            var choices = HubEquipment.Choices<IRotator>("rotator", selection => new HubRotatorDevice(selection, "missing.exe"), store);
            Assert.Equal(binding.Id, choices[0].Id); Assert.EndsWith("Configure.rotator", choices[1].Id);
            foreach (var choice in choices) ((IDisposable)choice).Dispose();
            foreach (var (property, json) in new[] { (HubRotatorProperty.IsMoving, "\"false\""), (HubRotatorProperty.Position, "360"),
                (HubRotatorProperty.Position, "359.999999999999"), (HubRotatorProperty.MechanicalPosition, "-1"),
                (HubRotatorProperty.StepSize, "0"), (HubRotatorProperty.StepSize, "1e-300"), (HubRotatorProperty.StepSize, "3.5e38") }) {
                using var document = JsonDocument.Parse(json);
                Assert.Throws<HubException>(() => HubRotatorProtocol.Validate(property, document.RootElement));
            }
            using var receipt = JsonDocument.Parse("{\"expectedTarget\":41.5,\"targetPosition\":41.5}");
            Assert.Equal((41.5f, 41.5f), HubRotatorProtocol.TargetReceipt(receipt.RootElement));
            using var malformed = JsonDocument.Parse("{\"expectedTarget\":41.5,\"targetPosition\":360}");
            Assert.Throws<HubException>(() => HubRotatorProtocol.TargetReceipt(malformed.RootElement));
            Assert.True(HubRotatorProtocol.AngularError(359.99f, 0) < 0.011f);
        } finally { Directory.Delete(directory, true); }
    }
}
