using System.Runtime.InteropServices;
using System.Text.Json;
using ASCOM.DeviceInterface;

namespace Regain.Hub.ASCOM;

[ComVisible(true), Guid("314f8626-f879-4830-aa36-f2d2e6d58608"), ClassInterface(ClassInterfaceType.None), ComDefaultInterface(typeof(ICoverCalibratorV2))]
public sealed class CoverCalibratorOutput : OutputDriver, ICoverCalibratorV2, ICoverCalibratorV1
{
    public CoverCalibratorOutput(HubSelection binding,string executable,string? workers = null)
        : base(SwitchOutput.Require(binding,"covercalibrator"),executable,workers) { }
    public override short InterfaceVersion => 2;
    private JsonElement Read(HubCoverCalibratorProperty property)
    {
        var value = Get(HubCoverCalibratorProtocol.Read(property),property.ToString());
        try { return HubCoverCalibratorProtocol.Validate(property,value); }
        catch (HubException) { throw new global::ASCOM.DriverException("Hub returned an invalid panel reading; inspect source health"); }
    }
    public int Brightness => Read(HubCoverCalibratorProperty.Brightness).GetInt32();
    public int MaxBrightness => Read(HubCoverCalibratorProperty.MaxBrightness).GetInt32();
    public CoverStatus CoverState => (CoverStatus)Read(HubCoverCalibratorProperty.CoverState).GetInt32();
    public CalibratorStatus CalibratorState => (CalibratorStatus)Read(HubCoverCalibratorProperty.CalibratorState).GetInt32();
    public bool CoverMoving => Read(HubCoverCalibratorProperty.CoverMoving).GetBoolean();
    public bool CalibratorChanging => Read(HubCoverCalibratorProperty.CalibratorChanging).GetBoolean();
    public void CalibratorOn(int Brightness)
    {
        object property;
        try { property = HubCoverCalibratorProtocol.On(Brightness); }
        catch (ArgumentOutOfRangeException) { throw new global::ASCOM.InvalidValueException(nameof(Brightness),"invalid","0..MaxBrightness"); }
        Put(property,nameof(CalibratorOn));
    }
    public void CalibratorOff() => Put(HubCoverCalibratorProtocol.Off(),nameof(CalibratorOff));
    public void OpenCover() => Put(HubCoverCalibratorProtocol.Open(),nameof(OpenCover));
    public void CloseCover() => Put(HubCoverCalibratorProtocol.Close(),nameof(CloseCover));
    public void HaltCover() => Put(HubCoverCalibratorProtocol.Halt(),nameof(HaltCover));
}
