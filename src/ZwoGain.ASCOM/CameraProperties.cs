using System.Collections;
using ASCOM.DeviceInterface;
namespace ZwoGain.Ascom;
public abstract partial class CameraBase
{
    public System.Int16 BinX { get => Client.Get<System.Int16>("binx"); set => Client.Put("binx", value); }
    public System.Int16 BinY { get => Client.Get<System.Int16>("biny"); set => Client.Put("biny", value); }
    public ASCOM.DeviceInterface.CameraStates CameraState { get => (ASCOM.DeviceInterface.CameraStates)Client.Get<int>("camerastate"); }
    public System.Int32 CameraXSize { get => Client.Get<System.Int32>("cameraxsize"); }
    public System.Int32 CameraYSize { get => Client.Get<System.Int32>("cameraysize"); }
    public System.Boolean CanAbortExposure { get => Client.Get<System.Boolean>("canabortexposure"); }
    public System.Boolean CanAsymmetricBin { get => Client.Get<System.Boolean>("canasymmetricbin"); }
    public System.Boolean CanGetCoolerPower { get => Client.Get<System.Boolean>("cangetcoolerpower"); }
    public System.Boolean CanPulseGuide { get => Client.Get<System.Boolean>("canpulseguide"); }
    public System.Boolean CanSetCCDTemperature { get => Client.Get<System.Boolean>("cansetccdtemperature"); }
    public System.Boolean CanStopExposure { get => Client.Get<System.Boolean>("canstopexposure"); }
    public System.Double CCDTemperature { get => Client.Get<System.Double>("ccdtemperature"); }
    public System.Boolean CoolerOn { get => Client.Get<System.Boolean>("cooleron"); set => Client.Put("cooleron", value); }
    public System.Double CoolerPower { get => Client.Get<System.Double>("coolerpower"); }
    public System.Double ElectronsPerADU { get => Client.Get<System.Double>("electronsperadu"); }
    public System.Double FullWellCapacity { get => Client.Get<System.Double>("fullwellcapacity"); }
    public System.Boolean HasShutter { get => Client.Get<System.Boolean>("hasshutter"); }
    public System.Double HeatSinkTemperature { get => Client.Get<System.Double>("heatsinktemperature"); }
    public System.Object ImageArray { get => Client.Image(false); }
    public System.Object ImageArrayVariant { get => Client.Image(true); }
    public System.Boolean ImageReady { get => Client.Get<System.Boolean>("imageready"); }
    public System.Boolean IsPulseGuiding { get => Client.Get<System.Boolean>("ispulseguiding"); }
    public System.Double LastExposureDuration { get => Client.Get<System.Double>("lastexposureduration"); }
    public System.String LastExposureStartTime { get => Client.Get<System.String>("lastexposurestarttime"); }
    public System.Int32 MaxADU { get => Client.Get<System.Int32>("maxadu"); }
    public System.Int16 MaxBinX { get => Client.Get<System.Int16>("maxbinx"); }
    public System.Int16 MaxBinY { get => Client.Get<System.Int16>("maxbiny"); }
    public System.Int32 NumX { get => Client.Get<System.Int32>("numx"); set => Client.Put("numx", value); }
    public System.Int32 NumY { get => Client.Get<System.Int32>("numy"); set => Client.Put("numy", value); }
    public System.Double PixelSizeX { get => Client.Get<System.Double>("pixelsizex"); }
    public System.Double PixelSizeY { get => Client.Get<System.Double>("pixelsizey"); }
    public System.Double SetCCDTemperature { get => Client.Get<System.Double>("setccdtemperature"); set => Client.Put("setccdtemperature", value); }
    public System.Int32 StartX { get => Client.Get<System.Int32>("startx"); set => Client.Put("startx", value); }
    public System.Int32 StartY { get => Client.Get<System.Int32>("starty"); set => Client.Put("starty", value); }
    public System.Int16 BayerOffsetX { get => Client.Get<System.Int16>("bayeroffsetx"); }
    public System.Int16 BayerOffsetY { get => Client.Get<System.Int16>("bayeroffsety"); }
    public System.Boolean CanFastReadout { get => Client.Get<System.Boolean>("canfastreadout"); }
    public System.Int16 Gain { get => Client.Get<System.Int16>("gain"); set => Client.Put("gain", value); }
    public System.Int16 GainMax { get => Client.Get<System.Int16>("gainmax"); }
    public System.Int16 GainMin { get => Client.Get<System.Int16>("gainmin"); }
    public System.Collections.ArrayList Gains { get => new ArrayList(Client.Get<string[]>("gains")); }
    public System.Int16 PercentCompleted { get => Client.Get<System.Int16>("percentcompleted"); }
    public System.Int16 ReadoutMode { get => Client.Get<System.Int16>("readoutmode"); set => Client.Put("readoutmode", value); }
    public System.Collections.ArrayList ReadoutModes { get => new ArrayList(Client.Get<string[]>("readoutmodes")); }
    public System.String SensorName { get => Client.Get<System.String>("sensorname"); }
    public ASCOM.DeviceInterface.SensorType SensorType { get => (ASCOM.DeviceInterface.SensorType)Client.Get<int>("sensortype"); }
    public System.Double ExposureMax { get => Client.Get<System.Double>("exposuremax"); }
    public System.Double ExposureMin { get => Client.Get<System.Double>("exposuremin"); }
    public System.Double ExposureResolution { get => Client.Get<System.Double>("exposureresolution"); }
    public System.Boolean FastReadout { get => Client.Get<System.Boolean>("fastreadout"); set => Client.Put("fastreadout", value); }
    public System.Int32 Offset { get => Client.Get<System.Int32>("offset"); set => Client.Put("offset", value); }
    public System.Int32 OffsetMax { get => Client.Get<System.Int32>("offsetmax"); }
    public System.Int32 OffsetMin { get => Client.Get<System.Int32>("offsetmin"); }
    public System.Collections.ArrayList Offsets { get => new ArrayList(Client.Get<string[]>("offsets")); }
    public System.Double SubExposureDuration { get => Client.Get<System.Double>("subexposureduration"); set => Client.Put("subexposureduration", value); }
    public System.Boolean Connecting { get => Client.Get<System.Boolean>("connecting"); }
}
