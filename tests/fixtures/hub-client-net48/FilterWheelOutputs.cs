using System.Text.Json;
using ASCOM.DeviceInterface;
using Regain.Hub;
using Regain.Hub.ASCOM;
using Regain.TestFixtures;

internal static partial class NativeOutputs
{
    internal static async Task SimulatedWheelRun(string executable,string path,Guid instance,JsonElement config,
        Guid source,HubEditorSession editor,CancellationToken token)
    {
        HubSelection Binding(int number)=>new(){ConfigPath=path,InstanceId=instance,DeviceType="filterwheel",Simulated=true,
            Label="Explicit simulation wheel",OutputId=config.GetProperty("outputs").EnumerateArray().Single(output=>
                output.GetProperty("number").GetInt32()==number && output.GetProperty("device").GetProperty("deviceType").GetString()=="filterwheel").GetProperty("id").GetGuid()};
        var controls=editor.SimulationControls(source); Require(controls.Count==6,"Wheel simulation descriptors");
        var offsets=controls.Single(c=>c.Path.SequenceEqual(new[]{"filterWheel","focusOffsets"}));
        Expect<InvalidOperationException>(()=>offsets.Parse("[1]")); Expect<InvalidOperationException>(()=>offsets.Parse("[0,2147483648]"));
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new{filterWheel=new{
            names=new[]{"L","Hα",""},focusOffsets=new[]{int.MinValue,0,int.MaxValue}}}),token);
        using var first=new FilterWheelOutput(Binding(8),executable); using var second=new FilterWheelOutput(Binding(9),executable);
        Query<IFilterWheelV3>(first); Query<IFilterWheelV2>(first);
        first.Connected=true; second.Connect(); await Until(()=>!second.Connecting,token);
        Require(first.Names.SequenceEqual(new[]{"L","Hα",""}) && second.FocusOffsets.SequenceEqual(new[]{int.MinValue,0,int.MaxValue}),"Wheel simulation metadata");
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new{fault="stalledMotion"}),token);
        first.Position=2; Require(second.Position==-1,"Wheel simulation nonblocking motion");
        Expect<global::ASCOM.DriverException>(()=>second.Position=1);
        first.Connected=false; Require(second.Connected && second.Position==-1,"Wheel disconnect changed sibling motion");
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new{fault="none"}),token);
        await Until(()=>second.Position==2,token);
        await Until(()=>second.DeviceState.Count==1 && second.DeviceState[0].Value is short position && position==2,token);
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new{fault="stoppedShort"}),token);
        second.Position=1;
        await Until(()=>second.Position!=-1,token); Require(second.Position==2,"Stopped-short wheel fabricated target");
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new{fault="invalidMotion"}),token);
        Expect<global::ASCOM.ValueNotSetException>(()=>_=second.Position);
        Expect<global::ASCOM.ValueNotSetException>(()=>second.Position=0);
        Require(second.Names.Length==3,"Invalid Position erased wheel metadata");
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new{fault="uncertainWrite"}),token);
        Expect<global::ASCOM.DriverException>(()=>second.Position=1);
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new{fault="none"}),token);
        Expect<global::ASCOM.DriverException>(()=>second.Position=0);
        while((await editor.SourceStatusAsync(source,token)).GetProperty("simulation").GetProperty("filterWheel").GetProperty("position").GetInt32()!=1) await Task.Delay(10,token);
        Require((await editor.SourceStatusAsync(source,token)).GetProperty("writeUncertain").GetBoolean(),"Fault clear removed wheel uncertainty");
        second.Connected=false;
        Console.WriteLine($"ASCOM simulated wheel {IntPtr.Size*8}-bit: shared array controls, current/legacy QI, timed Position, independent leases, stopped-short/malformed motion and retained uncertainty passed");
    }
    internal static async Task WheelRun(string executable,string path,Guid instance,JsonElement config,
        HubFilterWheelServer source,CancellationToken token)
    {
        HubSelection Binding(int index) => new() {ConfigPath=path,InstanceId=instance,
            OutputId=config.GetProperty("outputs")[index].GetProperty("id").GetGuid(),DeviceType="filterwheel",Label="Private wheel"};
        using var first=new FilterWheelOutput(Binding(10),executable);
        using var second=new FilterWheelOutput(Binding(11),executable);
        Query<IFilterWheelV3>(first); Query<IFilterWheelV2>(first);
        Require(first.InterfaceVersion==3 && !first.Connected && source.Moves==0,"Wheel metadata activated equipment");
        Require(OutputIdentity.ProgId(Binding(10)).StartsWith("Rgn.HL.") && OutputIdentity.ProgId(Binding(10)).Length==39,"Wheel stable identity");
        Expect<global::ASCOM.NotConnectedException>(()=>_=first.Position);
        first.Connected=true; second.Connect(); await Until(()=>!second.Connecting,token);
        Require(first.Names.SequenceEqual(new[]{"L","Hα",""}) && second.FocusOffsets.SequenceEqual(new[]{-12,0,17}),"Wheel live metadata");
        var names=first.Names; names[0]="local"; var offsets=first.FocusOffsets; offsets[0]=999;
        Require(second.Names[0]=="L" && second.FocusOffsets[0]==-12,"Wheel metadata arrays were shared with caller");
        source.Values["focusoffsets"]=new[]{int.MinValue,0,int.MaxValue};
        Require(first.FocusOffsets.SequenceEqual(new[]{int.MinValue,0,int.MaxValue}),"Wheel signed Int32 offsets");
        Expect<global::ASCOM.InvalidValueException>(()=>first.Position=-1);
        Expect<global::ASCOM.InvalidValueException>(()=>first.Position=3);
        Require(source.Moves==0,"Invalid wheel position dispatched");
        first.Position=2; Require(second.Position==-1 && source.Moves==1,"Wheel setter did not acknowledge nonblocking motion");
        Expect<global::ASCOM.DriverException>(()=>second.Position=1);
        source.Values["position"]=1; Require(first.Position==1,"Wheel fabricated requested target");
        source.Values["position"]=2;
        await Until(()=> {
            var state=first.DeviceState;
            return state.Count==1 && state[0].Name=="Position" && state[0].Value is short position && position==2;
        },token);
        AssertNoTimestamp(first.DeviceState);
        source.Values["position"]="2";
        Expect<global::ASCOM.ValueNotSetException>(()=>_=first.Position);
        Expect<global::ASCOM.ValueNotSetException>(()=>first.Position=0);
        Require(source.Moves==1 && second.Connected,"Malformed reading changed ownership or moved wheel");
        source.Values["position"]=2; source.Values["focusoffsets"]=new[]{0};
        Expect<global::ASCOM.ValueNotSetException>(()=>_=first.FocusOffsets);
        source.Values["focusoffsets"]=new[]{-12,0,17};
        first.Disconnect(); await Until(()=>!first.Connecting,token);
        Require(!first.Connected && second.Connected && second.Position==2,"Wheel disconnect revoked sibling lease");
        first.Connected=true; source.LoseMoveReply=true;
        Expect<global::ASCOM.DriverException>(()=>first.Position=0);
        Expect<global::ASCOM.DriverException>(()=>second.Position=1);
        Require(source.Moves==2 && source.Halts==0 && !first.Connected && !second.Connected,"Uncertain wheel command replayed or stole ownership");
        Require(first.SupportedActions.Count==0,"Wheel advertised unsupported calibration action");
        Expect<global::ASCOM.MethodNotImplementedException>(()=>first.Action("Regain.Calibrate",""));
        first.Connected=false; second.Connected=false;
        Console.WriteLine($"ASCOM wheel {IntPtr.Size*8}-bit: current/legacy QI, live metadata, short DeviceState, nonblocking shared motion, malformed reads and no-replay uncertainty passed");
    }
}
