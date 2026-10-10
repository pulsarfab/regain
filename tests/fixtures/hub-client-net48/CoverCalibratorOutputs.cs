using System.Text.Json;
using ASCOM.DeviceInterface;
using Regain.Hub;
using Regain.Hub.ASCOM;
using Regain.TestFixtures;

internal static partial class NativeOutputs
{
    internal static async Task PanelRun(string executable,string path,Guid instance,JsonElement config,
        HubCoverCalibratorServer source,int firstNumber,int secondNumber,int version,CancellationToken token)
    {
        HubSelection Binding(int number) => new() { ConfigPath=path,InstanceId=instance,DeviceType="covercalibrator",Label="Private panel",
            OutputId=config.GetProperty("outputs").EnumerateArray().Single(output=>output.GetProperty("number").GetInt32()==number &&
                output.GetProperty("device").TryGetProperty("deviceType",out var kind) && kind.GetString()=="covercalibrator").GetProperty("id").GetGuid() };
        using var first = new CoverCalibratorOutput(Binding(firstNumber),executable);
        using var second = new CoverCalibratorOutput(Binding(secondNumber),executable);
        Query<ICoverCalibratorV2>(first); Query<ICoverCalibratorV1>(first);
        // DeviceState returns declared enums to managed clients. Automation's
        // Object/VARIANT boundary carries the underlying Int32, not an enum name.
        foreach (var state in new object[]{CoverStatus.Unknown,CalibratorStatus.Ready}) {
            var variant=System.Runtime.InteropServices.Marshal.AllocCoTaskMem(24);
            try {
                System.Runtime.InteropServices.Marshal.GetNativeVariantForObject(state,variant);
                Require(System.Runtime.InteropServices.Marshal.ReadInt16(variant)==3,"Panel enum was not marshaled as VT_I4");
                var roundtrip=System.Runtime.InteropServices.Marshal.GetObjectForNativeVariant(variant);
                Require(roundtrip is int number && number==(int)Convert.ChangeType(state,typeof(int)),"Panel Automation state lost Int32 value");
            } finally { System.Runtime.InteropServices.Marshal.FreeCoTaskMem(variant); }
        }
        Require(first.InterfaceVersion==2 && !first.Connected && source.PanelCommands.IsEmpty,"Panel metadata activated equipment");
        Require(OutputIdentity.ProgId(Binding(firstNumber)).StartsWith("Rgn.HC.") && OutputIdentity.ProgId(Binding(firstNumber)).Length==39,"Panel stable identity");
        Expect<global::ASCOM.NotConnectedException>(()=>_=first.Brightness);
        first.Connected=true; second.Connect(); await Until(()=>!second.Connecting,token);
        Require(first.CoverState==CoverStatus.Closed && first.CalibratorState==CalibratorStatus.Off && second.MaxBrightness==4096 && first.Brightness==0,"Panel live typed properties");
        Expect<global::ASCOM.InvalidValueException>(()=>first.CalibratorOn(-1));
        Expect<global::ASCOM.InvalidValueException>(()=>first.CalibratorOn(4097));
        Require(source.PanelCommands.IsEmpty,"Invalid panel brightness dispatched");
        first.OpenCover(); Require(second.CoverState==CoverStatus.Moving && second.CoverMoving,"Cover command fabricated completion");
        second.CloseCover(); second.HaltCover();
        Require(first.CoverState==CoverStatus.Unknown,"Halt fabricated cover endpoint");
        if(version==1) Expect<global::ASCOM.ValueNotSetException>(()=>_=first.CoverMoving);
        else Require(!first.CoverMoving,"Known stopped modern cover lost completion");
        first.CalibratorOn(0);
        Require(second.Brightness==0 && second.CalibratorState==CalibratorStatus.NotReady && second.CalibratorChanging,"Zero-on or warm-up semantics lost");
        source.Values["calibratorstate"]=3; source.Values["calibratorchanging"]=false;
        await Until(()=> {
            var state=first.DeviceState;
            var items=Enumerable.Range(0,state.Count).Select(index=>state[index]).ToArray();
            return state.Count==(version==1?4:5) && items.Any(item=>item.Name=="CoverState" && item.Value is CoverStatus status && status==CoverStatus.Unknown) &&
                items.Any(item=>item.Name=="CalibratorState" && item.Value is CalibratorStatus status && status==CalibratorStatus.Ready);
        },token);
        var states=first.DeviceState;
        foreach(IStateValue item in states) {
            Require(item.Name!="MaxBrightness" && item.Name!="TimeStamp","Panel DeviceState included metadata/query timestamp");
            if(item.Name=="Brightness") Require(item.Value is int value && value==0,"Panel brightness lost Int32 type");
            if(item.Name=="CoverMoving" || item.Name=="CalibratorChanging") Require(item.Value is bool,"Panel completion lost Boolean type");
        }
        source.Values["maxbrightness"]=17;
        Require(second.MaxBrightness==17,"Panel maximum was cached as fixed metadata");
        Expect<global::ASCOM.InvalidValueException>(()=>first.CalibratorOn(18));
        source.Values["brightness"]="0";
        Expect<global::ASCOM.ValueNotSetException>(()=>_=second.Brightness);
        source.Values["brightness"]=0;
        source.Values["coverstate"]=0;
        Expect<global::ASCOM.MethodNotImplementedException>(()=>first.OpenCover());
        source.Values["coverstate"]=4;
        source.Values["calibratorstate"]=0;
        Expect<global::ASCOM.PropertyNotImplementedException>(()=>_=second.MaxBrightness);
        Expect<global::ASCOM.MethodNotImplementedException>(()=>first.CalibratorOn(0));
        Expect<global::ASCOM.MethodNotImplementedException>(()=>second.CalibratorOff());
        source.Values["calibratorstate"]=3;
        first.Disconnect(); await Until(()=>!first.Connecting,token);
        Require(!first.Connected && second.Connected && second.CalibratorState==CalibratorStatus.Ready,"Panel disconnect revoked sibling lease or darkened source");
        second.CalibratorOff(); Require(second.CalibratorState==CalibratorStatus.Off && second.Brightness==0,"Panel Off failed");
        first.Connected=true; source.LoseMoveReply=true;
        Expect<global::ASCOM.DriverException>(()=>first.CalibratorOn(17));
        Expect<global::ASCOM.DriverException>(()=>second.CalibratorOff());
        Expect<global::ASCOM.DriverException>(()=>second.HaltCover());
        Require(!first.Connected && !second.Connected,"Uncertain panel output remained connected");
        Require(source.PanelCommands.ToArray().SequenceEqual(new[]{"opencover","closecover","haltcover","calibratoron","calibratoroff","calibratoron"}),"Panel command replayed or cleanup actuated");
        first.Connected=false; second.Connected=false;
        Console.WriteLine($"ASCOM panel V{version} input {IntPtr.Size*8}-bit: current/legacy QI, live states/ranges, independent completion, zero-on, typed DeviceState, sibling leases and uncertain no-replay commands passed");
    }
}
