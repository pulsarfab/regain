using System.Text.Json;
namespace Regain.Core;

public sealed record Exposure(int width, int height, int bin, int x, int y, long microseconds, bool dark);
public sealed record Frame(ushort[] Pixels, int Width, int Height, DateTime StartedUtc, DateTime EndedUtc, int Recoveries,
    Exposure Exposure, IReadOnlyDictionary<int, long> Controls)
{
    public int RetainedReadRecoveries { get; init; }
}
public sealed record Control(int Type, long Min, long Max, long Value, bool Writable);
public sealed record CameraDescriptor(string Name, int Width, int Height, bool Color, int Bayer, double PixelSize,
    int BitDepth, bool Cooled, bool Shutter, int[] Bins)
{
    public int MinimumWidth { get; init; } = 8;
    public int MinimumHeight { get; init; } = 2;
    public int OriginAlignmentX { get; init; } = 1;
    public int OriginAlignmentY { get; init; } = 1;
    public static CameraDescriptor Parse(JsonElement j) => new(j.GetProperty("name").GetString()!, j.GetProperty("width").GetInt32(), j.GetProperty("height").GetInt32(),
        j.GetProperty("color").GetBoolean(), j.GetProperty("bayer").GetInt32(), j.GetProperty("pixelSize").GetDouble(), j.GetProperty("bitDepth").GetInt32(),
        j.GetProperty("cooled").GetBoolean(), j.GetProperty("shutter").GetBoolean(), j.GetProperty("bins").EnumerateArray().Select(v => v.GetInt32()).ToArray()) {
            MinimumWidth = j.TryGetProperty("minimumWidth", out var w) ? w.GetInt32() : 8,
            MinimumHeight = j.TryGetProperty("minimumHeight", out var h) ? h.GetInt32() : 2,
            OriginAlignmentX = j.TryGetProperty("originAlignment", out var x) ? x.GetInt32() : 1,
            OriginAlignmentY = j.TryGetProperty("originAlignment", out _) ? 2 : 1
        };
    // Coordinates are in binned pixels; backend restrictions are physical pixels.
    public Exposure NormalizeRoi(Exposure e)
    {
        if (!Bins.Contains(e.bin) || e.width <= 0 || e.height <= 0 || e.x < 0 || e.y < 0)
            throw new ArgumentOutOfRangeException(nameof(e));
        static (int origin, int size) Axis(int origin, int size, int sensor, int minimum, int alignment, int bin, int step) {
            if (minimum < 1 || alignment < 1) throw new InvalidDataException("Invalid camera ROI constraints");
            int gcd = bin, remainder = alignment;
            while (remainder != 0) (gcd, remainder) = (remainder, gcd % remainder);
            int quantum = alignment / gcd;
            int lower = checked((int)(((long)minimum + bin * step - 1) / (bin * step)) * step);
            int upper = sensor / bin / step * step;
            if (lower > upper) throw new InvalidDataException("Camera cannot fit the minimum ROI at this binning");
            size = Math.Clamp(size / step * step, lower, upper);
            origin = Math.Min(origin, sensor / bin - size) / quantum * quantum;
            return (origin, size);
        }
        var horizontal = Axis(e.x, e.width, Width, MinimumWidth, OriginAlignmentX, e.bin, 8);
        var vertical = Axis(e.y, e.height, Height, MinimumHeight, OriginAlignmentY, e.bin, 2);
        return e with {x=horizontal.origin, y=vertical.origin, width=horizontal.size, height=vertical.size};
    }
}
