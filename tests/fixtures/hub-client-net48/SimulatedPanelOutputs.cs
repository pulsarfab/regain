using System.Text.Json;
using ASCOM.DeviceInterface;
using Regain.Hub;
using Regain.Hub.ASCOM;

internal static partial class NativeOutputs
{
    internal static async Task SimulatedPanelRun(string executable,string path,Guid instance,JsonElement config,
        Guid source,HubEditorSession editor,CancellationToken token)
    {
        HubSelection Binding(int number)=>new(){ConfigPath=path,InstanceId=instance,DeviceType="covercalibrator",Simulated=true,
            Label="Explicit simulation panel",OutputId=config.GetProperty("outputs").EnumerateArray().Single(output=>
                output.GetProperty("number").GetInt32()==number && output.GetProperty("device").GetProperty("deviceType").GetString()=="covercalibrator").GetProperty("id").GetGuid()};
        var controls=editor.SimulationControls(source); Require(controls.Count==10,"Panel simulation descriptors");
        var brightness=controls.Single(c=>c.Path.SequenceEqual(new[]{"coverCalibrator","brightness"}));
        Expect<InvalidOperationException>(()=>brightness.Parse("1.5")); Expect<InvalidOperationException>(()=>brightness.Parse("2147483648"));
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new{fault="stalledMotion",
            coverCalibrator=new{moveDurationSeconds=300,lightDurationSeconds=0}}),token);
        using var first=new CoverCalibratorOutput(Binding(20),executable);
        using var second=new CoverCalibratorOutput(Binding(21),executable);
        Query<ICoverCalibratorV2>(first); Query<ICoverCalibratorV1>(first);
        first.Connected=true; second.Connect(); await Until(()=>!second.Connecting,token);
        first.OpenCover(); first.CalibratorOn(0);
        Require(second.CoverMoving && second.CalibratorChanging && second.Brightness==0 &&
            second.CalibratorState==CalibratorStatus.NotReady,"Panel simulation nonblocking independent operations");
        Expect<global::ASCOM.DriverException>(()=>second.CloseCover());
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new{fault="none"}),token);
        await Until(()=>!second.CalibratorChanging,token);
        Require(second.CoverMoving && second.CalibratorState==CalibratorStatus.Ready && second.Brightness==0,"Zero-on completion stopped cover or turned light off");
        await Until(()=> {
            var state=second.DeviceState;
            return state.Count==5 && Enumerable.Range(0,state.Count).Any(index=>state[index].Name=="CalibratorState" &&
                state[index].Value is CalibratorStatus status && status==CalibratorStatus.Ready);
        },token);
        first.Connected=false; Require(second.Connected && second.CoverMoving,"Panel disconnect stopped sibling motion");
        second.CalibratorOff(); Require(second.CoverMoving && second.CalibratorState==CalibratorStatus.Off,"Off halted cover");
        second.HaltCover(); Require(!second.CoverMoving && second.CoverState==CoverStatus.Unknown,"Halt invented endpoint");
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new{fault="invalidMotion"}),token);
        Expect<global::ASCOM.ValueNotSetException>(()=>_=second.CoverMoving);
        Require(second.CalibratorState==CalibratorStatus.Off,"Malformed completion erased light state");
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new{fault="uncertainWrite"}),token);
        Expect<global::ASCOM.DriverException>(()=>second.CalibratorOn(17));
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new{fault="none"}),token);
        Expect<global::ASCOM.DriverException>(()=>second.CalibratorOff());
        Expect<global::ASCOM.DriverException>(()=>second.HaltCover());
        var final=await editor.SourceStatusAsync(source,token);
        Require(final.GetProperty("writeUncertain").GetBoolean() && final.GetProperty("simulation").GetProperty("coverCalibrator").GetProperty("brightness").GetInt32()==17,
            "Clearing fault replayed command or actuated cleanup");
        second.Connected=false;
        Console.WriteLine($"ASCOM simulated panel {IntPtr.Size*8}-bit: shared controls, current/legacy QI, independent clocks, On(0), typed cache, malformed completion and retained uncertainty passed");
    }
}
