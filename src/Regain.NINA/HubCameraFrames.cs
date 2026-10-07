using System.Globalization;
using System.Text.Json;
using NINA.Core.Enum;
using NINA.Image.ImageData;
using Regain.Hub;

namespace Regain.NINA;

internal static class HubCameraFrames
{
    internal static ImageMetaData Metadata(JsonElement geometry, JsonElement exposure, SensorType sensor, int bayerX, int bayerY)
    {
        if (exposure.GetProperty("durationSeconds").ValueKind != JsonValueKind.Number || exposure.GetProperty("startTime").ValueKind != JsonValueKind.String ||
            exposure.GetProperty("durationError").ValueKind != JsonValueKind.Null || exposure.GetProperty("startTimeError").ValueKind != JsonValueKind.Null)
            throw new IOException("The completed camera frame has no authoritative exposure metadata");
        var metadata = new ImageMetaData();
        metadata.Camera.BinX = geometry.GetProperty("binX").GetInt32(); metadata.Camera.BinY = geometry.GetProperty("binY").GetInt32();
        metadata.Camera.BayerOffsetX = bayerX; metadata.Camera.BayerOffsetY = bayerY; metadata.Camera.SensorType = sensor;
        metadata.Image.ExposureTime = HubCameraProtocol.Validate(HubCameraProperty.LastExposureDuration, exposure.GetProperty("durationSeconds")).GetDouble();
        metadata.Image.ExposureStart = DateTime.Parse(HubCameraProtocol.Validate(HubCameraProperty.LastExposureStartTime, exposure.GetProperty("startTime")).GetString()!,
            CultureInfo.InvariantCulture, DateTimeStyles.AssumeUniversal | DateTimeStyles.AdjustToUniversal);
        metadata.Camera.BayerPattern = sensor switch { SensorType.RGGB => BayerPatternEnum.RGGB, SensorType.BGGR => BayerPatternEnum.BGGR,
            SensorType.GRBG => BayerPatternEnum.GRBG, SensorType.GBRG => BayerPatternEnum.GBRG, _ => BayerPatternEnum.None };
        return metadata;
    }
}
