using System.Text.Json;
using Regain.Rotator;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class OpticalWorkerRecoveryTests
{
    private sealed class Fixture : IDisposable
    {
        internal readonly string Root = Path.Combine(Path.GetTempPath(), "regain-optical-" + Guid.NewGuid().ToString("N"));
        internal string Worker => Path.Combine(Root, "OpticalWorkerFixture.exe");
        internal string State => Path.Combine(Root, "state");
        internal Fixture() {
            Directory.CreateDirectory(State);
            foreach (var file in Directory.GetFiles(Path.Combine(AppContext.BaseDirectory, "fixtures/optical-worker")))
                File.Copy(file, Path.Combine(Root, Path.GetFileName(file)));
        }
        internal void Fail(string kind) => File.WriteAllText(Path.Combine(State, kind + "-fail-once"), "");
        internal string[] Commands(string kind) => Directory.GetFiles(State, kind + "-*.jsonl").SelectMany(File.ReadAllLines)
            .Select(line => JsonDocument.Parse(line).RootElement.GetProperty("command").GetString()!).ToArray();
        public void Dispose() { try { Directory.Delete(Root, true); } catch (IOException) { } }
    }

    [Fact]
    public void WheelReopensAfterStaleHandleAndVerifiesSlotWithoutMoving()
    {
        using var fixture = new Fixture();
        using var session = new AccessorySession(fixture.Worker, "efw", Path.Combine(fixture.Root, "efw.json"));
        session.Select("0102030405060709"); session.Connect(); Assert.Equal(3, session.Status().Position);
        fixture.Fail("efw");
        Assert.Equal(3, session.Status().Position); Assert.True(session.Connected);
        Assert.Equal(2, fixture.Commands("efw").Count(c => c == "identity"));
        Assert.DoesNotContain("move", fixture.Commands("efw"));
    }

    [Fact]
    public void CaaReopensAndRestoresOnlyLogicalSyncWithoutMovingOrResettingOrigin()
    {
        using var fixture = new Fixture();
        using var session = new CaaSession(fixture.Worker, Path.Combine(fixture.Root, "caa.json"));
        session.Select("0102030405060708"); session.Connect(); Assert.Equal(30, session.Status().Mechanical);
        fixture.Fail("caa");
        Assert.Equal(30, session.Status().Mechanical); Assert.Equal(30, session.Status().Logical);
        var commands = fixture.Commands("caa");
        Assert.Equal(2, commands.Count(c => c == "identity"));
        Assert.DoesNotContain(commands, c => c.Contains("move") || c.Contains("reference") || c.Contains("origin"));
    }

    [Fact]
    public void CaaReverseChangeCannotBeHiddenByUnchangedAngles()
    {
        using var fixture = new Fixture();
        using var session = new CaaSession(fixture.Worker, Path.Combine(fixture.Root, "caa.json"));
        session.Select("0102030405060708"); session.Connect(); _ = session.Status();
        string path = Path.Combine(fixture.State, "caa-state.json");
        var state = System.Text.Json.Nodes.JsonNode.Parse(File.ReadAllText(path))!;
        state["reverse"] = true; File.WriteAllText(path, state.ToJsonString()); fixture.Fail("caa");
        Assert.Throws<IOException>(() => session.Status());
        Assert.False(session.Connected);
        Assert.DoesNotContain("reverse", fixture.Commands("caa"));
        Assert.DoesNotContain(fixture.Commands("caa"), command => command.Contains("move"));
    }

    [Fact]
    public void ChangedWheelSlotBlocksSubsequentReadsAndMovesWithoutModifyingSavedFilters()
    {
        using var fixture = new Fixture();
        using var session = new AccessorySession(fixture.Worker, "efw", Path.Combine(fixture.Root, "efw.json"));
        session.Select("0102030405060709"); session.Connect(); _ = session.Status();
        var profile = File.ReadAllText(session.ProfilePath);
        File.WriteAllText(Path.Combine(fixture.State, "efw-state.json"), "{\"position\":0,\"slots\":7,\"moving\":false,\"error\":0}");
        fixture.Fail("efw");
        Assert.Contains("Optical state", Assert.Throws<IOException>(() => session.Status()).Message);
        Assert.Throws<IOException>(() => session.Move(3));
        Assert.Equal(profile, File.ReadAllText(session.ProfilePath));
        Assert.DoesNotContain("move", fixture.Commands("efw"));
        session.Disconnect(); session.Connect(); Assert.Equal(0, session.Status().Position);
    }

    [Fact]
    public void LostWheelMoveAcknowledgementIsNeverReplayedOrHiddenByStatusRecovery()
    {
        using var fixture = new Fixture();
        using var session = new AccessorySession(fixture.Worker, "efw", Path.Combine(fixture.Root, "efw.json"));
        session.Select("0102030405060709"); session.Connect(); _ = session.Status();
        File.WriteAllText(Path.Combine(fixture.State, "efw-lose-move-ack"), "");
        Assert.Throws<IOException>(() => session.Move(4));
        Assert.Throws<IOException>(() => session.Status());
        Assert.Throws<IOException>(() => session.Move(4));
        Assert.Equal(1, fixture.Commands("efw").Count(c => c == "move"));
        Assert.Equal(1, fixture.Commands("efw").Count(c => c == "identity"));
    }

    [Fact]
    public void ManualDisconnectCannotBeUndoneByAnAutomaticStatusRead()
    {
        using var fixture = new Fixture();
        using var session = new AccessorySession(fixture.Worker, "efw", Path.Combine(fixture.Root, "efw.json"));
        session.Select("0102030405060709"); session.Connect(); _ = session.Status(); session.Disconnect();
        Assert.Throws<IOException>(() => session.Status());
        Assert.False(session.ConnectionRequested); Assert.False(session.Connected);
        Assert.Equal(1, fixture.Commands("efw").Count(c => c == "identity"));
    }

    [Fact]
    public void WorkerExitRecoversWithoutDiscardingEitherAscomClientLease()
    {
        using var fixture = new Fixture();
        using var session = new AccessorySession(fixture.Worker, "efw", Path.Combine(fixture.Root, "efw.json"));
        session.Select("0102030405060709");
        var shared = new SharedAccessoryDevice(session, (_, _) => { });
        Guid first = Guid.NewGuid(), second = Guid.NewGuid();
        shared.Connect(first, true); shared.Connect(second, true); _ = session.Status();
        var worker = (System.Diagnostics.Process)typeof(AccessorySession).GetField("worker", System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic)!.GetValue(session)!;
        worker.Kill(); Assert.True(worker.WaitForExit(5000));
        Assert.True(shared.Connected(first)); Assert.True(shared.Connected(second));
        Assert.Equal(3, session.Status().Position);
        Assert.True(shared.Connected(first)); Assert.True(shared.Connected(second));
        shared.Connect(first, false); Assert.True(shared.Connected(second));
        shared.Connect(second, false); Assert.False(session.Connected);
        Assert.DoesNotContain("move", fixture.Commands("efw"));
    }
}
