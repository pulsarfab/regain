using System.Text.Json;
using ASCOM.DeviceInterface;
using Regain.Hub;
using Regain.Hub.ASCOM;
using Regain.TestFixtures;

internal static partial class NativeOutputs
{
    internal static async Task SimulatedRotatorRun(string executable,string path,Guid instance,JsonElement config,
        Guid source,HubEditorSession editor,CancellationToken token)
    {
        HubSelection Binding(int number) => new() {ConfigPath=path,InstanceId=instance,DeviceType="rotator",Simulated=true,
            Label="Explicit simulation rotator",OutputId=config.GetProperty("outputs").EnumerateArray()
                .Single(output=>output.GetProperty("number").GetInt32()==number &&
                    output.GetProperty("device").GetProperty("deviceType").GetString()=="rotator").GetProperty("id").GetGuid()};
        var controls=editor.SimulationControls(source); Require(controls.Count==12,"Rotator simulation descriptors");
        var position=controls.Single(control=>control.Path.SequenceEqual(new[]{"rotator","position"}));
        Expect<InvalidOperationException>(()=>position.Parse("360")); Expect<InvalidOperationException>(()=>position.Parse("359.9999999"));
        var step=controls.Single(control=>control.Path.SequenceEqual(new[]{"rotator","stepSize"}));
        Expect<InvalidOperationException>(()=>step.Parse("1e-300"));
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new {rotator=new {
            position=20.0,mechanicalPosition=350.0,targetPosition=20.0}}),token);
        using var first=new RotatorOutput(Binding(8),executable);
        using var second=new RotatorOutput(Binding(9),executable);
        Query<IRotatorV4>(first); Query<IRotatorV3>(first); Query<IRotatorV2>(first);
        first.Connected=true; second.Connect(); await Until(()=>!second.Connecting,token);
        Require(first.Position==20 && second.MechanicalPosition==350,"Separate simulated coordinates");
        first.Move(-721.5f); await Until(()=>!second.IsMoving,token);
        Require(second.Position==18.5 && second.MechanicalPosition==348.5,"Signed timed relative motion");
        first.Sync(42.5f); second.Reverse=true;
        Require(second.Position==42.5 && second.MechanicalPosition==348.5 && first.Reverse,"Shared simulated Sync/Reverse");
        second.MoveAbsolute(50); await Until(()=>!first.IsMoving,token);
        Require(first.Position==50 && first.MechanicalPosition==356,"Logical move preserves Sync offset");
        first.MoveMechanical(12.25f); await Until(()=>!second.IsMoving,token);
        Require(second.Position==66.25 && second.MechanicalPosition==12.25,"Mechanical move preserves Sync offset");
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new {rotator=new {
            stepSizeAvailable=false,haltAvailable=false}}),token);
        Expect<global::ASCOM.PropertyNotImplementedException>(()=>_=first.StepSize);
        Expect<global::ASCOM.MethodNotImplementedException>(()=>first.Halt());
        first.Connected=false; Require(second.Connected && second.Position==66.25,"Disconnect stole simulated sibling lease");
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new {fault="uncertainWrite",rotator=new {
            position=0.0,mechanicalPosition=0.0,targetPosition=0.0,isMoving=false,haltAvailable=true}}),token);
        Expect<global::ASCOM.DriverException>(()=>second.Move(5));
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new {fault="none"}),token);
        Expect<global::ASCOM.DriverException>(()=>second.Halt());
        while ((await editor.SourceStatusAsync(source,token)).GetProperty("simulation").GetProperty("rotator").GetProperty("position").GetDouble()!=5)
            await Task.Delay(10,token);
        Require((await editor.SourceStatusAsync(source,token)).GetProperty("writeUncertain").GetBoolean(),"Clearing simulation fault cleared uncertainty");
        second.Connected=false;
        while ((await editor.SourceStatusAsync(source,token)).GetProperty("leaseCount").GetInt32()!=0) await Task.Delay(10,token);
        Console.WriteLine($"ASCOM simulated rotator {IntPtr.Size*8}-bit: shared descriptors, current/legacy QI, timed motion, source coordinates, optional errors and retained uncertainty passed");
    }
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
