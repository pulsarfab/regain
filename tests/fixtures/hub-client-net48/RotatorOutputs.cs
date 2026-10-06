using System.Text.Json;
using ASCOM.DeviceInterface;
using Regain.Hub;
using Regain.Hub.ASCOM;
using Regain.TestFixtures;

internal static partial class NativeOutputs
{
    internal static async Task RotatorRun(string executable, string path, Guid instance, JsonElement config, HubRotatorServer source, CancellationToken token)
    {
        HubSelection Binding(int index) => new() { ConfigPath = path, InstanceId = instance,
            OutputId = config.GetProperty("outputs")[index].GetProperty("id").GetGuid(), DeviceType = "rotator", Label = "Private rotator" };
        using var first = new RotatorOutput(Binding(6), executable);
        using var second = new RotatorOutput(Binding(7), executable);
        Query<IRotatorV4>(first); Query<IRotatorV3>(first); Query<IRotatorV2>(first);
        Require(first.InterfaceVersion == 4 && !first.Connected && source.Moves == 0, "Rotator metadata activated equipment");
        Require(OutputIdentity.ProgId(Binding(6)).StartsWith("Rgn.HR.") && OutputIdentity.ProgId(Binding(6)).Length == 39, "Rotator stable COM identity");
        Expect<global::ASCOM.NotConnectedException>(() => _ = first.Position);
        first.Connected = true; second.Connect(); await Until(() => !second.Connecting, token);
        Require(first.CanReverse && !first.Reverse && first.Position == 20 && first.MechanicalPosition == 350 && first.StepSize == 0.02f, "Rotator typed properties");
        first.Sync(42.5f); second.Reverse = true;
        Require(first.Reverse && second.Position == 42.5f && second.MechanicalPosition == 350, "Source-owned shared Sync/Reverse");
        Expect<global::ASCOM.InvalidValueException>(() => first.MoveAbsolute(360));
        Expect<global::ASCOM.InvalidValueException>(() => first.Move(float.NaN));
        Require(source.Moves == 0, "Invalid rotator command dispatched");
        first.Move(-721.5f);
        Require(second.IsMoving && second.TargetPosition == 41 && second.Position == 41, "Signed relative start acknowledgment");
        Expect<global::ASCOM.DriverException>(() => second.MoveAbsolute(50));
        second.Halt(); Require(!first.IsMoving && source.Halts == 1, "Explicit shared rotator Halt");
        second.MoveMechanical(12.25f); first.Halt();
        Require(second.MechanicalPosition == 12.25f && source.Halts == 2, "Mechanical coordinate command");
        first.MoveAbsolute(50); source.Values["ismoving"] = false;
        await Until(() => {
            var state = first.DeviceState;
            if (state.Count != 3) return false;
            for (var i = 0; i < state.Count; i++) {
                var item = state[i];
                if (item.Name == "Position" && item.Value is float position && position == 50) return true;
            }
            return false;
        }, token);
        AssertNoTimestamp(first.DeviceState);
        source.Values.TryRemove("stepsize", out _);
        Expect<global::ASCOM.PropertyNotImplementedException>(() => _ = first.StepSize);
        source.Values["position"] = 360.0;
        Expect<global::ASCOM.ValueNotSetException>(() => _ = first.Position);
        Require(second.MechanicalPosition < 360, "Invalid logical reading erased mechanical data");
        source.Values["position"] = 50.0;
        first.Disconnect(); await Until(() => !first.Connecting, token);
        Require(!first.Connected && second.Connected && second.Position == 50 && source.Halts == 2, "Disconnect revoked or halted another owner");
        first.Connected = true; source.LoseMoveReply = true;
        Expect<global::ASCOM.DriverException>(() => first.MoveAbsolute(70));
        Expect<global::ASCOM.DriverException>(() => second.MoveAbsolute(80));
        Require(source.Moves == 4 && source.Halts == 2 && !first.Connected && !second.Connected, "Uncertain move was replayed, halted or rebound");
        first.Connected = false; second.Connected = false;
        source.LoseMoveReply = false; source.Values["canreverse"] = false;
        Expect<global::ASCOM.ValueNotSetException>(() => first.Connected = true);
        Console.WriteLine($"ASCOM rotator {IntPtr.Size * 8}-bit: current/legacy QI, strict typed state, all commands, independent leases, optional errors, asynchronous connection and lost-reply fencing passed");
    }
}
