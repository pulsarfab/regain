using System.Collections;
using ASCOM.DeviceInterface;
namespace ZwoGain.Ascom;
public abstract partial class CameraBase
{
    public System.Int16 BinX { get => Client.BinX; set => Client.BinX = value; }
    public System.Int16 BinY { get => Client.BinY; set => Client.BinY = value; }
    public ASCOM.DeviceInterface.CameraStates CameraState { get => (ASCOM.DeviceInterface.CameraStates)Client.CameraState; }
    public System.Int32 CameraXSize { get => Client.CameraXSize; }
    public System.Int32 CameraYSize { get => Client.CameraYSize; }
    public System.Boolean CanAbortExposure { get => Client.CanAbortExposure; }
    public System.Boolean CanAsymmetricBin { get => Client.CanAsymmetricBin; }
    public System.Boolean CanGetCoolerPower { get => Client.CanGetCoolerPower; }
    public System.Boolean CanPulseGuide { get => Client.CanPulseGuide; }
    public System.Boolean CanSetCCDTemperature { get => Client.CanSetCCDTemperature; }
    public System.Boolean CanStopExposure { get => Client.CanStopExposure; }
    public System.Double CCDTemperature { get => Client.CCDTemperature; }
    public System.Boolean CoolerOn { get => Client.CoolerOn; set => Client.CoolerOn = value; }
    public System.Double CoolerPower { get => Client.CoolerPower; }
    public System.Double ElectronsPerADU { get => Client.ElectronsPerADU; }
    public System.Double FullWellCapacity { get => Client.FullWellCapacity; }
    public System.Boolean HasShutter { get => Client.HasShutter; }
    public System.Double HeatSinkTemperature { get => Client.HeatSinkTemperature; }
    public System.Object ImageArray { get => Client.ImageArray; }
    public System.Object ImageArrayVariant { get => Client.ImageArrayVariant; }
    public System.Boolean ImageReady { get => Client.ImageReady; }
    public System.Boolean IsPulseGuiding { get => Client.IsPulseGuiding; }
    public System.Double LastExposureDuration { get => Client.LastExposureDuration; }
    public System.String LastExposureStartTime { get => Client.LastExposureStartTime; }
    public System.Int32 MaxADU { get => Client.MaxADU; }
    public System.Int16 MaxBinX { get => Client.MaxBinX; }
    public System.Int16 MaxBinY { get => Client.MaxBinY; }
    public System.Int32 NumX { get => Client.NumX; set => Client.NumX = value; }
    public System.Int32 NumY { get => Client.NumY; set => Client.NumY = value; }
    public System.Double PixelSizeX { get => Client.PixelSizeX; }
    public System.Double PixelSizeY { get => Client.PixelSizeY; }
    public System.Double SetCCDTemperature { get => Client.SetCCDTemperature; set => Client.SetCCDTemperature = value; }
    public System.Int32 StartX { get => Client.StartX; set => Client.StartX = value; }
    public System.Int32 StartY { get => Client.StartY; set => Client.StartY = value; }
    public System.Int16 BayerOffsetX { get => Client.BayerOffsetX; }
    public System.Int16 BayerOffsetY { get => Client.BayerOffsetY; }
    public System.Boolean CanFastReadout { get => Client.CanFastReadout; }
    public System.Int16 Gain { get => Client.Gain; set => Client.Gain = value; }
    public System.Int16 GainMax { get => Client.GainMax; }
    public System.Int16 GainMin { get => Client.GainMin; }
    public System.Collections.ArrayList Gains { get => new ArrayList(Client.Gains.ToArray()); }
    public System.Int16 PercentCompleted { get => Client.PercentCompleted; }
    public System.Int16 ReadoutMode { get => Client.ReadoutMode; set => Client.ReadoutMode = value; }
    public System.Collections.ArrayList ReadoutModes { get => new ArrayList(Client.ReadoutModes.ToArray()); }
    public System.String SensorName { get => Client.SensorName; }
    public ASCOM.DeviceInterface.SensorType SensorType { get => (ASCOM.DeviceInterface.SensorType)Client.SensorType; }
    public System.Double ExposureMax { get => Client.ExposureMax; }
    public System.Double ExposureMin { get => Client.ExposureMin; }
    public System.Double ExposureResolution { get => Client.ExposureResolution; }
    public System.Boolean FastReadout { get => Client.FastReadout; set => Client.FastReadout = value; }
    public System.Int32 Offset { get => Client.Offset; set => Client.Offset = value; }
    public System.Int32 OffsetMax { get => Client.OffsetMax; }
    public System.Int32 OffsetMin { get => Client.OffsetMin; }
    public System.Collections.ArrayList Offsets { get => new ArrayList(Client.Offsets.ToArray()); }
    public System.Double SubExposureDuration { get => Client.SubExposureDuration; set => Client.SubExposureDuration = value; }
    public System.Boolean Connecting { get => Client.Connecting; }
}
