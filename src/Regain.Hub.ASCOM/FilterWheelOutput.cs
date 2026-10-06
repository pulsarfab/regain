using System.Runtime.InteropServices;
using System.Text.Json;
using ASCOM.DeviceInterface;

namespace Regain.Hub.ASCOM;

[ComVisible(true), Guid("06487c5a-70de-4d30-81ba-f5cf5e3f6ff0"), ClassInterface(ClassInterfaceType.None), ComDefaultInterface(typeof(IFilterWheelV3))]
public sealed class FilterWheelOutput : OutputDriver, IFilterWheelV3, IFilterWheelV2
{
    public FilterWheelOutput(HubSelection binding,string executable,string? workers = null)
        : base(SwitchOutput.Require(binding,"filterwheel"),executable,workers) { }
    public override short InterfaceVersion => 3;
    private JsonElement Read(HubFilterWheelProperty property)
    {
        var value = Get(HubFilterWheelProtocol.Read(property),property.ToString());
        try {return HubFilterWheelProtocol.Validate(property,value);}
        catch (HubException) {throw new global::ASCOM.DriverException("Hub returned an invalid filter wheel reading; inspect source health");}
    }
    public string[] Names => Read(HubFilterWheelProperty.Names).EnumerateArray().Select(item => item.GetString()!).ToArray();
    public int[] FocusOffsets => Read(HubFilterWheelProperty.FocusOffsets).EnumerateArray().Select(item => item.GetInt32()).ToArray();
    public short Position {
        get => checked((short)Read(HubFilterWheelProperty.Position).GetInt32());
        set {
            object property;
            try {property = HubFilterWheelProtocol.Move(value);}
            catch (ArgumentOutOfRangeException) {throw new global::ASCOM.InvalidValueException("Position","invalid","configured wheel slots");}
            Put(property,"Position",false);
        }
    }
}
