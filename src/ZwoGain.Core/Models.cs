using System.Text.Json;
namespace ZwoGain.Core;

public sealed record Exposure(int width, int height, int bin, int x, int y, long microseconds, bool dark);
public sealed record Frame(ushort[] Pixels, int Width, int Height, DateTime StartedUtc, DateTime EndedUtc, int Recoveries,
    Exposure Exposure, IReadOnlyDictionary<int, long> Controls);
public sealed record Control(int Type, long Min, long Max, long Value, bool Writable);
public sealed record CameraDescriptor(string Name, int Width, int Height, bool Color, int Bayer, double PixelSize,
    int BitDepth, bool Cooled, bool Shutter, int[] Bins)
{
    public static CameraDescriptor Parse(JsonElement j) => new(j.GetProperty("name").GetString()!, j.GetProperty("width").GetInt32(), j.GetProperty("height").GetInt32(),
        j.GetProperty("color").GetBoolean(), j.GetProperty("bayer").GetInt32(), j.GetProperty("pixelSize").GetDouble(), j.GetProperty("bitDepth").GetInt32(),
        j.GetProperty("cooled").GetBoolean(), j.GetProperty("shutter").GetBoolean(), j.GetProperty("bins").EnumerateArray().Select(v => v.GetInt32()).ToArray());
}
public sealed record RecoveryOptions
{
    public int MaxRetries { get; init; } = 3;
    public double MaximumRetryExposureSeconds { get; init; } = 30;
    public double ReconnectDelaySeconds { get; init; } = 5;
    public double CommandTimeoutSeconds { get; init; } = 15;
    public double DownloadTimeoutSeconds { get; init; } = 60;
    public double ExposureGraceSeconds { get; init; } = 30;
    public double CoolingTimeoutSeconds { get; init; } = 300;
    public double TemperatureToleranceC { get; init; } = 2;
    public int CoolingStableSamples { get; init; } = 3;
    public double CoolingSampleSeconds { get; init; } = 2;
    // Experimental: SDK has no documented transfer resume contract. Never enabled implicitly.
    public int ReadyFrameDownloadRetries { get; init; } = 0;
    public int DirectReadRetries { get; init; } = 2;
    public void Validate()
    {
        if (!double.IsFinite(MaximumRetryExposureSeconds) || MaximumRetryExposureSeconds < 0 || MaximumRetryExposureSeconds > 86400)
            throw new ArgumentOutOfRangeException(nameof(MaximumRetryExposureSeconds));
        if (MaxRetries is < 0 or > 20 || ReadyFrameDownloadRetries is < 0 or > 5 || DirectReadRetries is < 0 or > 5 || CoolingStableSamples is < 1 or > 60)
            throw new ArgumentOutOfRangeException(nameof(MaxRetries));
        foreach (double v in new[] { ReconnectDelaySeconds, CommandTimeoutSeconds, DownloadTimeoutSeconds, ExposureGraceSeconds, CoolingTimeoutSeconds, TemperatureToleranceC, CoolingSampleSeconds })
            if (!double.IsFinite(v) || v <= 0 || v > 3600)
                throw new ArgumentOutOfRangeException(nameof(v));
    }
}
