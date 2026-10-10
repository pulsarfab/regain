using System.Collections;
using System.Runtime.InteropServices;
using System.Text.Json;
using ASCOM.DeviceInterface;

namespace Regain.Hub.ASCOM;

[ComVisible(true), Guid("4ec10928-be26-4cef-98bf-ebba7d45e95a"), ClassInterface(ClassInterfaceType.None), ComDefaultInterface(typeof(ICameraV4))]
public sealed class CameraOutput : OutputDriver, ICameraV4, ICameraV3, ICameraV2
{
    public CameraOutput(HubSelection binding, string executable, string? workers = null)
        : base(SwitchOutput.Require(binding, "camera"), executable, workers) { }
    public override short InterfaceVersion => 4;
    private JsonElement Read(HubCameraProperty property) {
        var value = Get(HubCameraProtocol.Read(property), HubCameraProtocol.AscomName(property));
        try { return HubCameraProtocol.Validate(property, value); }
        catch (HubException) { throw new global::ASCOM.DriverException("Hub returned an invalid camera reading; inspect source health"); }
    }
    private short Short(HubCameraProperty property) {
        try { return checked((short)Read(property).GetInt32()); }
        catch (OverflowException) { throw new global::ASCOM.DriverException("Hub camera value exceeds the ASCOM Int16 range"); }
    }
    private int Integer(HubCameraProperty property) => Read(property).GetInt32();
    private double Number(HubCameraProperty property) => Read(property).GetDouble();
    private bool Boolean(HubCameraProperty property) => Read(property).GetBoolean();
    private ArrayList Strings(HubCameraProperty property) => new(Read(property).EnumerateArray().Select(value => value.GetString()!).ToArray());
    private void Setting(HubCameraProperty property, object value) {
        object command;
        try { command = HubCameraProtocol.Setting(property, value); }
        catch (ArgumentOutOfRangeException) { throw new global::ASCOM.InvalidValueException(HubCameraProtocol.AscomName(property), "invalid", "camera limits"); }
        Put(command, HubCameraProtocol.AscomName(property), false);
    }
    public void StartExposure(double Duration, bool Light) {
        object command;
        try { command = HubCameraProtocol.Start(Duration, Light); }
        catch (ArgumentOutOfRangeException) { throw new global::ASCOM.InvalidValueException("Duration", "invalid", "finite nonnegative seconds"); }
        Put(command, "StartExposure");
    }
    public void StopExposure() => Put(new { member = "stopExposure" }, "StopExposure");
    public void AbortExposure() => Put(new { member = "abortExposure" }, "AbortExposure");
    public void PulseGuide(GuideDirections Direction, int Duration) {
        object command;
        try { command = HubCameraProtocol.Guide((int)Direction, Duration); }
        catch (ArgumentOutOfRangeException) { throw new global::ASCOM.InvalidValueException("PulseGuide", "invalid", "direction 0-3 and nonnegative milliseconds"); }
        Put(command, "PulseGuide");
    }
    private object Image(bool variants) {
        var member = variants ? "ImageArrayVariant" : "ImageArray";
        try { using var image = DownloadCameraImage(member); return HubCameraArrays.Convert(image, variants); }
        catch (Exception error) { throw Translate(error, member, false, false); }
    }
    public object ImageArray => Image(false);
    public object ImageArrayVariant => Image(true);
    public short BayerOffsetX => Short(HubCameraProperty.BayerOffsetX);
    public short BayerOffsetY => Short(HubCameraProperty.BayerOffsetY);
    public short BinX { get => Short(HubCameraProperty.BinX); set => Setting(HubCameraProperty.BinX, value); }
    public short BinY { get => Short(HubCameraProperty.BinY); set => Setting(HubCameraProperty.BinY, value); }
    public CameraStates CameraState => (CameraStates)Integer(HubCameraProperty.CameraState);
    public int CameraXSize => Integer(HubCameraProperty.CameraXSize);
    public int CameraYSize => Integer(HubCameraProperty.CameraYSize);
    public bool CanAbortExposure => Boolean(HubCameraProperty.CanAbortExposure);
    public bool CanAsymmetricBin => Boolean(HubCameraProperty.CanAsymmetricBin);
    public bool CanFastReadout => Boolean(HubCameraProperty.CanFastReadout);
    public bool CanGetCoolerPower => Boolean(HubCameraProperty.CanGetCoolerPower);
    public bool CanPulseGuide => Boolean(HubCameraProperty.CanPulseGuide);
    public bool CanSetCCDTemperature => Boolean(HubCameraProperty.CanSetCcdTemperature);
    public bool CanStopExposure => Boolean(HubCameraProperty.CanStopExposure);
    public double CCDTemperature => Number(HubCameraProperty.CcdTemperature);
    public bool CoolerOn { get => Boolean(HubCameraProperty.CoolerOn); set => Setting(HubCameraProperty.CoolerOn, value); }
    public double CoolerPower => Number(HubCameraProperty.CoolerPower);
    public double ElectronsPerADU => Number(HubCameraProperty.ElectronsPerAdu);
    public double ExposureMin => Number(HubCameraProperty.ExposureMin);
    public double ExposureMax => Number(HubCameraProperty.ExposureMax);
    public double ExposureResolution => Number(HubCameraProperty.ExposureResolution);
    public bool FastReadout { get => Boolean(HubCameraProperty.FastReadout); set => Setting(HubCameraProperty.FastReadout, value); }
    public double FullWellCapacity => Number(HubCameraProperty.FullWellCapacity);
    public short Gain { get => Short(HubCameraProperty.Gain); set => Setting(HubCameraProperty.Gain, value); }
    public short GainMin => Short(HubCameraProperty.GainMin);
    public short GainMax => Short(HubCameraProperty.GainMax);
    public ArrayList Gains => Strings(HubCameraProperty.Gains);
    public bool HasShutter => Boolean(HubCameraProperty.HasShutter);
    public double HeatSinkTemperature => Number(HubCameraProperty.HeatSinkTemperature);
    public bool ImageReady => Boolean(HubCameraProperty.ImageReady);
    public bool IsPulseGuiding => Boolean(HubCameraProperty.IsPulseGuiding);
    public double LastExposureDuration => Number(HubCameraProperty.LastExposureDuration);
    public string LastExposureStartTime => Read(HubCameraProperty.LastExposureStartTime).GetString()!;
    public int MaxADU => Integer(HubCameraProperty.MaxAdu);
    public short MaxBinX => Short(HubCameraProperty.MaxBinX);
    public short MaxBinY => Short(HubCameraProperty.MaxBinY);
    public int NumX { get => Integer(HubCameraProperty.NumX); set => Setting(HubCameraProperty.NumX, value); }
    public int NumY { get => Integer(HubCameraProperty.NumY); set => Setting(HubCameraProperty.NumY, value); }
    public int Offset { get => Integer(HubCameraProperty.Offset); set => Setting(HubCameraProperty.Offset, value); }
    public int OffsetMin => Integer(HubCameraProperty.OffsetMin);
    public int OffsetMax => Integer(HubCameraProperty.OffsetMax);
    public ArrayList Offsets => Strings(HubCameraProperty.Offsets);
    public short PercentCompleted => Short(HubCameraProperty.PercentCompleted);
    public double PixelSizeX => Number(HubCameraProperty.PixelSizeX);
    public double PixelSizeY => Number(HubCameraProperty.PixelSizeY);
    public short ReadoutMode { get => Short(HubCameraProperty.ReadoutMode); set => Setting(HubCameraProperty.ReadoutMode, value); }
    public ArrayList ReadoutModes => Strings(HubCameraProperty.ReadoutModes);
    public string SensorName => Read(HubCameraProperty.SensorName).GetString()!;
    public SensorType SensorType => (SensorType)Integer(HubCameraProperty.SensorType);
    public double SetCCDTemperature { get => Number(HubCameraProperty.SetCcdTemperature); set => Setting(HubCameraProperty.SetCcdTemperature, value); }
    public int StartX { get => Integer(HubCameraProperty.StartX); set => Setting(HubCameraProperty.StartX, value); }
    public int StartY { get => Integer(HubCameraProperty.StartY); set => Setting(HubCameraProperty.StartY, value); }
    public double SubExposureDuration { get => Number(HubCameraProperty.SubExposureDuration); set => Setting(HubCameraProperty.SubExposureDuration, value); }
}
