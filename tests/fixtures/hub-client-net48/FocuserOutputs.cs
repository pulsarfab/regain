using System.Text.Json;
using ASCOM.DeviceInterface;
using Regain.Hub;
using Regain.Hub.ASCOM;
using Regain.TestFixtures;

internal static partial class NativeOutputs
{
    internal static async Task SimulatedFocuserRun(string executable,string path,Guid instance,JsonElement config,
        Guid source,HubEditorSession editor,CancellationToken token)
    {
        var binding = new HubSelection { ConfigPath=path,InstanceId=instance,
            OutputId=config.GetProperty("outputs")[5].GetProperty("id").GetGuid(),DeviceType="focuser",Label="Explicit simulation focuser",Simulated=true };
        using var driver = new FocuserOutput(binding,executable);
        var controls=editor.SimulationControls(source); Require(controls.Count==15,"Focuser simulation descriptors");
        var position=controls.Single(control=>control.Path.SequenceEqual(new[]{"focuser","position"}));
        Require(position.Type=="integer" && position.Parse("50100").GetInt32()==50100,"Int32 simulation control");
        Expect<InvalidOperationException>(()=>position.Parse("1.5")); Expect<InvalidOperationException>(()=>position.Parse("2147483648"));
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new {focuser=new {position=50100}}),token);
        driver.Connected=true;
        Require(driver.Position==50100 && driver.MaxStep==100000 && driver.StepSize==1.25,"Simulator typed focuser properties");
        driver.Move(50200); await Until(()=>!driver.IsMoving,token);
        Require(driver.Position==50200,"Simulator did not complete ASCOM move");
        driver.TempComp=true; Require(driver.TempComp,"Simulator TempComp");
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new {
            focuser=new {temperatureAvailable=false,stepSizeAvailable=false,haltAvailable=false}}),token);
        Expect<global::ASCOM.PropertyNotImplementedException>(()=>_=driver.Temperature);
        Expect<global::ASCOM.PropertyNotImplementedException>(()=>_=driver.StepSize);
        Expect<global::ASCOM.MethodNotImplementedException>(()=>driver.Halt());
        var status=await editor.SourceStatusAsync(source,token); Require(status.GetProperty("simulated").GetBoolean(),"Simulator lost its identity");
        driver.Connected=false;
        while ((await editor.SourceStatusAsync(source,token)).GetProperty("leaseCount").GetInt32()!=0) await Task.Delay(10,token);
        Console.WriteLine($"ASCOM simulated focuser {IntPtr.Size*8}-bit: shared Int32 setup, timed motion, TempComp, optional errors and lease cleanup passed");
    }
    internal static async Task FocuserRun(string executable, string path, Guid instance, JsonElement config, HubFocuserServer source, CancellationToken token)
    {
        HubSelection Binding(int index) => new() { ConfigPath = path, InstanceId = instance,
            OutputId = config.GetProperty("outputs")[index].GetProperty("id").GetGuid(), DeviceType = "focuser",
            Label = "Private focuser", Simulated = false };
        using var first = new FocuserOutput(Binding(3), executable);
        using var second = new FocuserOutput(Binding(4), executable);
        Query<IFocuserV4>(first); Query<IFocuserV3>(first); Query<IFocuserV2>(first);
        Require(first.InterfaceVersion == 4 && !first.Connected && !first.Link && source.Moves == 0, "Focuser metadata activated equipment");
        Require(OutputIdentity.ProgId(Binding(3)).StartsWith("Rgn.HF.") && OutputIdentity.ProgId(Binding(3)).Length == 39, "Focuser COM identity");
        Expect<global::ASCOM.NotConnectedException>(() => _ = first.Position);
        first.Link = true;
        second.Connect(); await Until(() => !second.Connecting, token);
        Require(first.Absolute && first.MaxStep == 1000 && first.MaxIncrement == 100 && first.Position == 50, "Focuser integer properties");
        Require(first.TempComp && first.TempCompAvailable && first.Temperature == -5, "Focuser boolean/temperature properties");
        Expect<global::ASCOM.PropertyNotImplementedException>(() => _ = first.StepSize);
        Expect<global::ASCOM.InvalidValueException>(() => first.Move(200));
        Require(source.Moves == 0, "Invalid move reached upstream");
        first.Move(70); // Return after start, while the device is still moving.
        Require(source.Moves == 1 && second.IsMoving && second.Position == 70, "Focuser Move blocked or lost shared state");
        Expect<global::ASCOM.DriverException>(() => second.Move(80));
        second.Halt(); Require(!first.IsMoving && source.Halts == 1, "Focuser Halt");
        first.TempComp = false; Require(!second.TempComp, "Shared temperature compensation");
        await Until(() => {
            var states = first.DeviceState;
            for (var index = 0; index < states.Count; index++)
                if (states[index].Name == "Position" && states[index].Value is int position && position == 70) return true;
            return false;
        }, token);
        AssertNoTimestamp(first.DeviceState);
        first.Disconnect(); await Until(() => !first.Connecting, token);
        Require(!first.Connected && second.Connected && second.Position == 70 && source.Halts == 1, "Disconnect halted or revoked a sibling");
        second.Connected = false;
        source.Values["absolute"] = false;
        first.Connected = true;
        Require(!first.Absolute, "Relative focuser became absolute");
        Expect<global::ASCOM.PropertyNotImplementedException>(() => _ = first.Position);
        first.Move(-20); first.Halt();
        Require(source.Moves == 2 && (int)source.Values["position"] == -20, "Relative signed movement lost");
        first.Connected = false;
        source.Values["absolute"] = true; source.Values["position"] = 50;
        first.Connected = true; second.Connected = true;
        source.LoseMoveReply = true;
        Expect<global::ASCOM.DriverException>(() => first.Move(70));
        Expect<global::ASCOM.DriverException>(() => second.Move(80));
        Require(source.Moves == 3 && source.Halts == 2 && !first.Connected && !second.Connected, "Lost command was replayed, halted or rebound");
        first.Connected = true; Require(!first.Connected, "Idempotent connect adopted a replacement generation");
        first.Connected = false; second.Connected = false;
        Console.WriteLine($"ASCOM focuser {IntPtr.Size * 8}-bit: current/legacy COM QI, typed state, absolute/relative moves, shared leases, Link, asynchronous connection and lost-reply fencing passed");
    }
}
