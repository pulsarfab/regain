using System.Runtime.InteropServices;
using ASCOM.DeviceInterface;

namespace Regain.Hub.ASCOM;

[ComVisible(true), Guid("e17366ba-1523-5987-98d5-897217ef7fd7"), ClassInterface(ClassInterfaceType.None), ComDefaultInterface(typeof(ISwitchV3))]
public sealed class SwitchOutput : OutputDriver, ISwitchV3, ISwitchV2
{
    public SwitchOutput(HubSelection binding, string executable, string? workers = null) : base(Require(binding, "switch"), executable, workers) { }
    internal static HubSelection Require(HubSelection binding, string type)
    {
        if (binding.DeviceType != type) throw new global::ASCOM.InvalidValueException("Hub output class mismatch");
        return binding;
    }
    public override short InterfaceVersion => 3;
    public short MaxSwitch => checked((short)Get(new { member = "maxSwitch" }, "MaxSwitch").GetInt32());
    private static uint Id(short id) => id < 0 ? throw new global::ASCOM.InvalidValueException("Id", id.ToString(), "0..MaxSwitch-1") : (uint)id;
    private static double Finite(double value)
        => double.IsNaN(value) || double.IsInfinity(value) ? throw new global::ASCOM.InvalidValueException("Value", "non-finite", "finite configured range") : value;
    public string GetSwitchName(short id) => Get(new { member = "getSwitchName", id = Id(id) }, "GetSwitchName", true).GetString()!;
    public void SetSwitchName(short id, string name) { Id(id); throw new global::ASCOM.MethodNotImplementedException("SetSwitchName; edit the shared hub configuration"); }
    public string GetSwitchDescription(short id) => Get(new { member = "getSwitchDescription", id = Id(id) }, "GetSwitchDescription", true).GetString()!;
    public bool GetSwitch(short id) => Get(new { member = "getSwitch", id = Id(id) }, "GetSwitch", true).GetBoolean();
    public double GetSwitchValue(short id) => Get(new { member = "getSwitchValue", id = Id(id) }, "GetSwitchValue", true).GetDouble();
    public bool CanWrite(short id) => Get(new { member = "canWrite", id = Id(id) }, "CanWrite", true).GetBoolean();
    public double MinSwitchValue(short id) => Get(new { member = "minSwitchValue", id = Id(id) }, "MinSwitchValue", true).GetDouble();
    public double MaxSwitchValue(short id) => Get(new { member = "maxSwitchValue", id = Id(id) }, "MaxSwitchValue", true).GetDouble();
    public double SwitchStep(short id) => Get(new { member = "switchStep", id = Id(id) }, "SwitchStep", true).GetDouble();
    public void SetSwitch(short id, bool state) => Put(new { member = "setSwitch", id = Id(id), state }, "SetSwitch");
    public void SetSwitchValue(short id, double value) => Put(new { member = "setSwitchValue", id = Id(id), value = Finite(value) }, "SetSwitchValue");
    public bool CanAsync(short id) => Get(new { member = "canAsync", id = Id(id) }, "CanAsync", true).GetBoolean();
    public bool StateChangeComplete(short id) => Get(new { member = "stateChangeComplete", id = Id(id) }, "StateChangeComplete", true).GetBoolean();
    public void SetAsync(short id, bool state) => Put(new { member = "setAsync", id = Id(id), state }, "SetAsync");
    public void SetAsyncValue(short id, double value) => Put(new { member = "setAsyncValue", id = Id(id), value = Finite(value) }, "SetAsyncValue");
    public void CancelAsync(short id) => Put(new { member = "cancelAsync", id = Id(id) }, "CancelAsync");
}

[ComVisible(true), Guid("2b39f660-c59e-5b1b-bf5b-f11fd520cb38"), ClassInterface(ClassInterfaceType.None), ComDefaultInterface(typeof(ISafetyMonitorV3))]
public sealed class SafetyOutput : OutputDriver, ISafetyMonitorV3, ISafetyMonitor
{
    public SafetyOutput(HubSelection binding, string executable, string? workers = null) : base(SwitchOutput.Require(binding, "safetymonitor"), executable, workers) { }
    public override short InterfaceVersion => 3;
    public bool IsSafe => Get(new { member = "isSafe" }, "IsSafe").GetBoolean();
}

[ComVisible(true), Guid("8efff11b-0925-5e28-aade-e632549f07a5"), ClassInterface(ClassInterfaceType.None), ComDefaultInterface(typeof(IObservingConditionsV2))]
public sealed class WeatherOutput : OutputDriver, IObservingConditionsV2, IObservingConditions
{
    public WeatherOutput(HubSelection binding, string executable, string? workers = null) : base(SwitchOutput.Require(binding, "observingconditions"), executable, workers) { }
    public override short InterfaceVersion => 2;
    public double AveragePeriod {
        get => Get(new { member = "averagePeriod" }, "AveragePeriod").GetDouble();
        set {
            if (double.IsNaN(value) || double.IsInfinity(value) || value < 0) throw new global::ASCOM.InvalidValueException("AveragePeriod", "invalid", "finite non-negative hours");
            Put(new { member = "averagePeriod", hours = value }, "AveragePeriod", false);
        }
    }
    private double Measurement(string property) => Get(new { member = "measurement", metric = property.ToLowerInvariant() }, property).GetProperty("value").GetDouble();
    public double CloudCover => Measurement("CloudCover");
    public double DewPoint => Measurement("DewPoint");
    public double Humidity => Measurement("Humidity");
    public double Pressure => Measurement("Pressure");
    public double RainRate => Measurement("RainRate");
    public double SkyBrightness => Measurement("SkyBrightness");
    public double SkyQuality => Measurement("SkyQuality");
    public double StarFWHM => Measurement("StarFWHM");
    public double SkyTemperature => Measurement("SkyTemperature");
    public double Temperature => Measurement("Temperature");
    public double WindDirection => Measurement("WindDirection");
    public double WindGust => Measurement("WindGust");
    public double WindSpeed => Measurement("WindSpeed");
    public double TimeSinceLastUpdate(string propertyName) => Get(new { member = "timeSinceLastUpdate", sensor = Sensor(propertyName) }, "TimeSinceLastUpdate", true).GetDouble();
    public string SensorDescription(string propertyName) => Get(new { member = "sensorDescription", sensor = Sensor(propertyName) }, "SensorDescription", true).GetString()!;
    private static string Sensor(string property) => property ?? throw new global::ASCOM.InvalidValueException("PropertyName", "null", "weather property name");
    public void Refresh() => Put(new { member = "refresh" }, "Refresh");
}
