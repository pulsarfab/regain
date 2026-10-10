using System.Runtime.InteropServices;
using System.Text.Json;
using ASCOM.DeviceInterface;

namespace Regain.Hub.ASCOM;

[ComVisible(true), Guid("84c7ef4a-8b23-4b34-baf8-997a1e9c5f21"), ClassInterface(ClassInterfaceType.None), ComDefaultInterface(typeof(IFocuserV4))]
public sealed class FocuserOutput : OutputDriver, IFocuserV4, IFocuserV3, IFocuserV2
{
    public FocuserOutput(HubSelection binding, string executable, string? workers = null) : base(SwitchOutput.Require(binding, "focuser"), executable, workers) { }
    public override short InterfaceVersion => 4;
    private JsonElement Read(HubFocuserProperty property)
    {
        var value = Get(HubFocuserProtocol.Read(property), property.ToString());
        try { return HubFocuserProtocol.Validate(property, value); }
        catch (HubException) { throw new global::ASCOM.DriverException("Hub returned an invalid focuser reading; inspect source health"); }
    }
    public bool Absolute => Read(HubFocuserProperty.Absolute).GetBoolean();
    public int MaxStep => Read(HubFocuserProperty.MaxStep).GetInt32();
    public int MaxIncrement => Read(HubFocuserProperty.MaxIncrement).GetInt32();
    public int Position => Read(HubFocuserProperty.Position).GetInt32();
    public bool IsMoving => Read(HubFocuserProperty.IsMoving).GetBoolean();
    public bool TempCompAvailable => Read(HubFocuserProperty.TempCompAvailable).GetBoolean();
    public bool TempComp {
        get => Read(HubFocuserProperty.TempComp).GetBoolean();
        set => Put(HubFocuserProtocol.TempComp(value), "TempComp", false);
    }
    public double Temperature => Read(HubFocuserProperty.Temperature).GetDouble();
    public double StepSize => Read(HubFocuserProperty.StepSize).GetDouble();
    public bool Link { get => Connected; set => Connected = value; }
    public void Move(int position) => Put(HubFocuserProtocol.Move(position), "Move");
    public void Halt() => Put(HubFocuserProtocol.Halt(), "Halt");
}
