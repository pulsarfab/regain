using System.Text.Json;

namespace Regain.Hub;

public enum HubCameraProperty {
    BayerOffsetX, BayerOffsetY, BinX, BinY, CameraState, CameraXSize, CameraYSize,
    CanAbortExposure, CanAsymmetricBin, CanFastReadout, CanGetCoolerPower,
    CanPulseGuide, CanSetCcdTemperature, CanStopExposure, CcdTemperature,
    CoolerOn, CoolerPower, ElectronsPerAdu, ExposureMin, ExposureMax,
    ExposureResolution, FastReadout, FullWellCapacity, Gain, GainMin, GainMax,
    Gains, HasShutter, HeatSinkTemperature, ImageReady, IsPulseGuiding,
    LastExposureDuration, LastExposureStartTime, MaxAdu, MaxBinX, MaxBinY,
    NumX, NumY, Offset, OffsetMin, OffsetMax, Offsets, PercentCompleted,
    PixelSizeX, PixelSizeY, ReadoutMode, ReadoutModes, SensorName, SensorType,
    SetCcdTemperature, StartX, StartY, SubExposureDuration
}
public enum HubCameraValueKind { Boolean, Integer, Number, Text, Strings }

/// Scalar camera contract shared by Windows imports and output providers.
/// Acquisition ownership, geometry admission and image lifetime belong to Rust.
public static class HubCameraProtocol {
    public static string Key(HubCameraProperty property) {
        if (!Enum.IsDefined(typeof(HubCameraProperty), property)) throw new ArgumentOutOfRangeException(nameof(property));
        var name = property.ToString();
        return char.ToLowerInvariant(name[0]) + name.Substring(1);
    }
    public static string Member(HubCameraProperty property) => Key(property).ToLowerInvariant();
    public static string AscomName(HubCameraProperty property) => property switch {
        HubCameraProperty.CcdTemperature => "CCDTemperature",
        HubCameraProperty.CanSetCcdTemperature => "CanSetCCDTemperature",
        HubCameraProperty.SetCcdTemperature => "SetCCDTemperature",
        HubCameraProperty.ElectronsPerAdu => "ElectronsPerADU",
        HubCameraProperty.MaxAdu => "MaxADU", _ => property.ToString()
    };
    public static HubCameraValueKind Kind(HubCameraProperty property) => property switch {
        HubCameraProperty.CanAbortExposure or HubCameraProperty.CanAsymmetricBin or
        HubCameraProperty.CanFastReadout or HubCameraProperty.CanGetCoolerPower or
        HubCameraProperty.CanPulseGuide or HubCameraProperty.CanSetCcdTemperature or
        HubCameraProperty.CanStopExposure or HubCameraProperty.CoolerOn or
        HubCameraProperty.FastReadout or HubCameraProperty.HasShutter or
        HubCameraProperty.ImageReady or HubCameraProperty.IsPulseGuiding => HubCameraValueKind.Boolean,
        HubCameraProperty.CcdTemperature or HubCameraProperty.CoolerPower or
        HubCameraProperty.ElectronsPerAdu or HubCameraProperty.ExposureMin or
        HubCameraProperty.ExposureMax or HubCameraProperty.ExposureResolution or
        HubCameraProperty.FullWellCapacity or HubCameraProperty.HeatSinkTemperature or
        HubCameraProperty.LastExposureDuration or HubCameraProperty.PixelSizeX or
        HubCameraProperty.PixelSizeY or HubCameraProperty.SetCcdTemperature or
        HubCameraProperty.SubExposureDuration => HubCameraValueKind.Number,
        HubCameraProperty.LastExposureStartTime or HubCameraProperty.SensorName => HubCameraValueKind.Text,
        HubCameraProperty.Gains or HubCameraProperty.Offsets or HubCameraProperty.ReadoutModes => HubCameraValueKind.Strings,
        _ when Enum.IsDefined(typeof(HubCameraProperty), property) => HubCameraValueKind.Integer,
        _ => throw new ArgumentOutOfRangeException(nameof(property))
    };
    public static JsonElement Validate(HubCameraProperty property, JsonElement value) {
        var valid = false;
        switch (Kind(property)) {
            case HubCameraValueKind.Boolean:
                valid = value.ValueKind is JsonValueKind.True or JsonValueKind.False; break;
            case HubCameraValueKind.Integer:
                valid = value.ValueKind == JsonValueKind.Number && value.TryGetInt32(out var integer)
                    && (property switch {
                        HubCameraProperty.BinX or HubCameraProperty.BinY or HubCameraProperty.MaxBinX or
                        HubCameraProperty.MaxBinY or HubCameraProperty.CameraXSize or HubCameraProperty.CameraYSize or
                        HubCameraProperty.NumX or HubCameraProperty.NumY => integer > 0,
                        HubCameraProperty.CameraState or HubCameraProperty.SensorType => integer >= 0 && integer <= 5,
                        HubCameraProperty.PercentCompleted => integer >= 0 && integer <= 100,
                        HubCameraProperty.BayerOffsetX or HubCameraProperty.BayerOffsetY or HubCameraProperty.StartX or
                        HubCameraProperty.StartY or HubCameraProperty.ReadoutMode or HubCameraProperty.MaxAdu => integer >= 0,
                        _ => true
                    }); break;
            case HubCameraValueKind.Number:
                valid = value.ValueKind == JsonValueKind.Number && value.TryGetDouble(out var number)
                    && !double.IsNaN(number) && !double.IsInfinity(number) && (property switch {
                        HubCameraProperty.CoolerPower => number >= 0 && number <= 100,
                        HubCameraProperty.ExposureResolution or HubCameraProperty.PixelSizeX or HubCameraProperty.PixelSizeY => number > 0,
                        HubCameraProperty.ElectronsPerAdu or HubCameraProperty.FullWellCapacity or HubCameraProperty.ExposureMin or
                        HubCameraProperty.ExposureMax or HubCameraProperty.LastExposureDuration or HubCameraProperty.SubExposureDuration => number >= 0,
                        _ => true
                    }); break;
            case HubCameraValueKind.Text:
                valid = value.ValueKind == JsonValueKind.String && (property != HubCameraProperty.LastExposureStartTime
                    || ValidStartTime(value.GetString()!)); break;
            case HubCameraValueKind.Strings:
                valid = value.ValueKind == JsonValueKind.Array && value.GetArrayLength() is > 0 and <= 1024
                    && value.EnumerateArray().All(item => item.ValueKind == JsonValueKind.String); break;
        }
        return valid ? value : throw new HubException(HubFailure.Protocol);
    }
    private static bool ValidStartTime(string value) {
        if (value.Length > 128) return false;
        if (value.EndsWith("Z", StringComparison.Ordinal)) value = value.Substring(0, value.Length - 1);
        else if (value.EndsWith("+00:00", StringComparison.Ordinal)) value = value.Substring(0, value.Length - 6);
        if (value.Length < 19 || value[4] != '-' || value[7] != '-' || value[10] != 'T'
            || value[13] != ':' || value[16] != ':' || value.Length > 19 && (value[19] != '.' || value.Length == 20)) return false;
        for (var i = 0; i < value.Length; i++) {
            if (i is 4 or 7 or 10 or 13 or 16 or 19) continue;
            if (value[i] < '0' || value[i] > '9') return false;
        }
        int Number(int start, int count) => int.Parse(value.Substring(start, count), System.Globalization.CultureInfo.InvariantCulture);
        var year = Number(0, 4); var month = Number(5, 2); var day = Number(8, 2);
        var days = month switch { 2 => year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) ? 29 : 28,
            4 or 6 or 9 or 11 => 30, >= 1 and <= 12 => 31, _ => 0 };
        return day >= 1 && day <= days && Number(11, 2) <= 23 && Number(14, 2) <= 59 && Number(17, 2) <= 60;
    }
}
