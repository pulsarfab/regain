using System.Collections;
using System.IO;
using System.Text.Json;

namespace Regain.Hub.ASCOM;

internal sealed partial class ImportDriver {
    private static HubCameraProperty? CameraProperty(string member) {
        foreach (HubCameraProperty property in Enum.GetValues(typeof(HubCameraProperty)))
            if (HubCameraProtocol.Member(property) == member) return property;
        return null;
    }
    private object ReadCamera(HubCameraProperty property, JsonElement parameters) {
        Fields(parameters);
        var raw = Get(HubCameraProtocol.AscomName(property));
        object result;
        switch (HubCameraProtocol.Kind(property)) {
            case HubCameraValueKind.Boolean: result = Boolean(raw); break;
            case HubCameraValueKind.Integer: result = Int32(raw); break;
            case HubCameraValueKind.Number: result = Number(raw); break;
            case HubCameraValueKind.Text: result = Text(raw); break;
            case HubCameraValueKind.Strings:
                result = ReadStrings(raw,1024); break;
            default: throw new Unsupported();
        }
        try { HubCameraProtocol.Validate(property, JsonSerializer.SerializeToElement(result)); }
        catch (HubException) { throw new BadValue(); }
        return result;
    }
    private static string[] ReadStrings(object? raw, int maximum) {
        if (raw is not IList list || list.Count < 1 || list.Count > maximum || raw is Array array && array.Rank != 1) throw new BadValue();
        var names = new string[list.Count]; var index = 0; long bytes = 0;
        var encoding = new System.Text.UTF8Encoding(false,true);
        foreach (var item in list) {
            if (item is not string text) throw new BadValue();
            try { bytes += encoding.GetByteCount(text); }
            catch (System.Text.EncoderFallbackException) { throw new BadValue(); }
            if (bytes > 1024 * 1024) throw new BadValue();
            names[index++] = text;
        }
        return names;
    }
    private object? WriteCamera(string member, JsonElement parameters) {
        if (member == "startexposure") {
            Fields(parameters, "Duration", "Light");
            var duration = InputNumber(Parameter(parameters, "Duration"));
            var light = Parameter(parameters, "Light");
            if (duration < 0 || light.ValueKind is not (JsonValueKind.True or JsonValueKind.False)) throw new InvalidInput();
            Call("StartExposure", duration, light.GetBoolean()); return null;
        }
        if (member is "stopexposure" or "abortexposure") {
            Fields(parameters); Call(member == "stopexposure" ? "StopExposure" : "AbortExposure"); return null;
        }
        var property = CameraProperty(member) ?? throw new Unsupported();
        var name = HubCameraProtocol.AscomName(property);
        switch (property) {
            case HubCameraProperty.BinX: case HubCameraProperty.BinY:
            case HubCameraProperty.NumX: case HubCameraProperty.NumY:
            case HubCameraProperty.StartX: case HubCameraProperty.StartY:
            case HubCameraProperty.Gain: case HubCameraProperty.Offset: case HubCameraProperty.ReadoutMode:
                Fields(parameters, name);
                var item = Parameter(parameters, name);
                if (item.ValueKind != JsonValueKind.Number || !item.TryGetInt32(out var integer)
                    || property is HubCameraProperty.BinX or HubCameraProperty.BinY or HubCameraProperty.NumX or HubCameraProperty.NumY && integer <= 0
                    || property is HubCameraProperty.StartX or HubCameraProperty.StartY or HubCameraProperty.ReadoutMode && integer < 0) throw new InvalidInput();
                if (property is HubCameraProperty.BinX or HubCameraProperty.BinY or HubCameraProperty.Gain or HubCameraProperty.ReadoutMode) {
                    if (integer < short.MinValue || integer > short.MaxValue) throw new InvalidInput();
                    Set(name, (short)integer);
                } else Set(name, integer);
                return null;
            case HubCameraProperty.CoolerOn: case HubCameraProperty.FastReadout:
                Fields(parameters, name);
                var boolean = Parameter(parameters, name);
                if (boolean.ValueKind is not (JsonValueKind.True or JsonValueKind.False)) throw new InvalidInput();
                Set(name, boolean.GetBoolean()); return null;
            case HubCameraProperty.SetCcdTemperature: case HubCameraProperty.SubExposureDuration:
                Fields(parameters, name);
                var number = InputNumber(Parameter(parameters, name));
                if (property == HubCameraProperty.SetCcdTemperature ? number < -273.15 : number < 0) throw new InvalidInput();
                Set(name, number); return null;
            default: throw new Unsupported();
        }
    }
}

/// A detached numeric SAFEARRAY, never a COM object. The STA retrieves it once;
/// the protocol thread copies its contiguous ASCOM [X,Y,plane] storage in bounded
/// chunks without another driver call or another image-sized allocation.
internal sealed class ComCameraImage {
    private const int MaximumBytes = 512 * 1024 * 1024;
    internal const int ChunkBytes = 64 * 1024;
    internal static readonly byte[] Terminator = System.Text.Encoding.ASCII.GetBytes("RGNIMAGE");
    private readonly Array pixels;
    private readonly int width, height, planes, rank, type, length;
    internal object Descriptor { get; }
    internal ComCameraImage(object? raw) {
        if (raw is not Array array || array.Rank is not (2 or 3)) throw new ArgumentException();
        var element = array.GetType().GetElementType();
        (type, var size, var name) = element == typeof(short) ? (1, 2, "int16")
            : element == typeof(int) ? (2, 4, "int32") : element == typeof(double) ? (3, 8, "double")
            : element == typeof(float) ? (4, 4, "single") : element == typeof(ulong) ? (5, 8, "uInt64")
            : element == typeof(byte) ? (6, 1, "byte") : element == typeof(long) ? (7, 8, "int64")
            : element == typeof(ushort) ? (8, 2, "uInt16") : element == typeof(uint) ? (9, 4, "uInt32")
            : throw new ArgumentException();
        pixels = array; rank = array.Rank;
        width = array.GetLength(0); height = array.GetLength(1); planes = rank == 3 ? array.GetLength(2) : 1;
        if (width <= 0 || height <= 0 || planes <= 0 || array.LongLength > MaximumBytes / size) throw new ArgumentException();
        length = (int)(array.LongLength * size);
        Descriptor = new { width, height, planes = rank == 3 ? (int?)planes : null,
            elementType = name, transmissionType = name, order = "ascom" };
    }
    internal void WriteTo(Stream output, long requestId) {
        // The finite body has an exact ImageBytes header and an explicit trailer.
        // A receiver validates both before returning to newline scalar framing.
        using var writer = new BinaryWriter(output, System.Text.Encoding.UTF8, leaveOpen: true);
        foreach (var field in new[] { 1, 0, (int)(requestId & int.MaxValue), 0, 44, type, type, rank, width, height, rank == 3 ? planes : 0 }) writer.Write(field);
        var buffer = new byte[Math.Min(ChunkBytes, length)];
        for (var offset = 0; offset < length;) {
            var count = Math.Min(buffer.Length, length - offset);
            Buffer.BlockCopy(pixels, offset, buffer, 0, count);
            output.Write(buffer, 0, count); offset += count;
        }
        output.Write(Terminator, 0, Terminator.Length);
        output.Flush();
    }
}
