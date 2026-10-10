using System.Text.Json;

namespace Regain.Hub;

public enum HubImageElementType { Int16 = 1, Int32 = 2, Double = 3, Single = 4, UInt64 = 5, Byte = 6, Int64 = 7, UInt16 = 8, UInt32 = 9 }

/// Limits retained encoded pixels and explicitly charged array conversions.
/// Share across camera sessions. COM's own marshaling/caller memory is external.
public sealed class HubImageBudget
{
    public const int MaximumImageBytes = 512 * 1024 * 1024;
    public static HubImageBudget Shared { get; } = new(MaximumImageBytes);
    public long MaximumBytes { get; }
    public long UsedBytes => Interlocked.Read(ref used);
    private long used;
    public HubImageBudget(long maximumBytes)
    {
        if (maximumBytes <= 0 || maximumBytes > MaximumImageBytes) throw new ArgumentOutOfRangeException(nameof(maximumBytes));
        MaximumBytes = maximumBytes;
    }
    internal Reservation Reserve(int count)
    {
        if (count <= 0 || count > MaximumImageBytes) throw new HubException(HubFailure.Protocol);
        while (true) {
            var before = UsedBytes;
            if (count > MaximumBytes - before) throw new HubException(HubFailure.Busy);
            if (Interlocked.CompareExchange(ref used, before + count, before) != before) continue;
            try { return new Reservation(this, count); }
            catch { Interlocked.Add(ref used, -count); throw; }
        }
    }
    internal sealed class Reservation(HubImageBudget owner, int count) : IDisposable
    {
        private HubImageBudget? budget = owner;
        public void Dispose()
        {
            var previous = Interlocked.Exchange(ref budget, null);
            if (previous is not null) Interlocked.Add(ref previous.used, -count);
        }
    }
}

public sealed class HubImageDescriptor
{
    public int Width { get; }
    public int Height { get; }
    public int? Planes { get; }
    public int Rank => Planes.HasValue ? 3 : 2;
    public HubImageElementType ElementType { get; }
    public HubImageElementType TransmissionType { get; }
    public int ByteLength { get; }
    internal HubImageDescriptor(JsonElement value)
    {
        HubWire.Members(value, "width", "height", "planes", "elementType", "transmissionType", "order");
        Width = value.GetProperty("width").GetInt32(); Height = value.GetProperty("height").GetInt32();
        var planes = value.GetProperty("planes"); Planes = planes.ValueKind == JsonValueKind.Null ? null : planes.GetInt32();
        ElementType = Type(value.GetProperty("elementType").GetString());
        TransmissionType = Type(value.GetProperty("transmissionType").GetString());
        if (Width <= 0 || Height <= 0 || Planes <= 0 || value.GetProperty("order").GetString() != "ascom" ||
            ElementType != TransmissionType && !(ElementType == HubImageElementType.Int32 &&
                TransmissionType is HubImageElementType.Byte or HubImageElementType.Int16 or HubImageElementType.UInt16))
            throw new HubException(HubFailure.Protocol);
        var bytes = checked((long)Width * Height * (Planes ?? 1) * Size(TransmissionType));
        if (bytes > HubImageBudget.MaximumImageBytes) throw new HubException(HubFailure.Protocol);
        ByteLength = checked((int)bytes);
    }
    internal static int Size(HubImageElementType type) => type switch {
        HubImageElementType.Byte => 1,
        HubImageElementType.Int16 or HubImageElementType.UInt16 => 2,
        HubImageElementType.Int32 or HubImageElementType.UInt32 or HubImageElementType.Single => 4,
        HubImageElementType.Int64 or HubImageElementType.UInt64 or HubImageElementType.Double => 8,
        _ => throw new HubException(HubFailure.Protocol)
    };
    private static HubImageElementType Type(string? value) => value switch {
        "int16" => HubImageElementType.Int16, "int32" => HubImageElementType.Int32,
        "double" => HubImageElementType.Double, "single" => HubImageElementType.Single,
        "uInt64" => HubImageElementType.UInt64, "byte" => HubImageElementType.Byte,
        "int64" => HubImageElementType.Int64, "uInt16" => HubImageElementType.UInt16,
        "uInt32" => HubImageElementType.UInt32, _ => throw new HubException(HubFailure.Protocol)
    };
}

/// Frozen identities from the control client and completed acquisition.
public abstract class HubImageIdentity
{
    public Guid HostInstance { get; }
    public Guid ConfigurationRevision { get; }
    public Guid Source { get; }
    public Guid Generation { get; }
    public Guid Acquisition { get; }
    internal HubImageIdentity(Guid hostInstance, Guid configurationRevision, Guid source, Guid generation, Guid acquisition)
    {
        if (new[] { hostInstance, configurationRevision, source, generation, acquisition }.Any(id => id == Guid.Empty))
            throw new HubException(HubFailure.InvalidRequest);
        HostInstance = hostInstance; ConfigurationRevision = configurationRevision;
        Source = source; Generation = generation; Acquisition = acquisition;
    }
    internal abstract string OperationKey { get; }
    internal abstract object Wire();
    internal abstract void Match(JsonElement value);
}

public sealed class HubImageRequest : HubImageIdentity
{
    public Guid ClientId { get; }
    public Guid Output { get; }
    public HubImageRequest(Guid hostInstance, Guid configurationRevision, Guid clientId,
        Guid output, Guid source, Guid generation, Guid acquisition)
        : base(hostInstance, configurationRevision, source, generation, acquisition)
    {
        if (clientId == Guid.Empty || output == Guid.Empty)
            throw new HubException(HubFailure.InvalidRequest);
        ClientId = clientId; Output = output;
    }
    internal override string OperationKey => "cameraImage";
    internal override object Wire() => new { hostInstance = HostInstance, configurationRevision = ConfigurationRevision,
        clientId = ClientId, output = Output, source = Source, generation = Generation, acquisition = Acquisition };
    internal override void Match(JsonElement value)
    {
        HubWire.Members(value, "hostInstance", "configurationRevision", "clientId", "output", "source", "generation", "acquisition");
        if (HubWire.Identity(value, "hostInstance") != HostInstance || HubWire.Identity(value, "configurationRevision") != ConfigurationRevision ||
            HubWire.Identity(value, "clientId") != ClientId || HubWire.Identity(value, "output") != Output ||
            HubWire.Identity(value, "source") != Source || HubWire.Identity(value, "generation") != Generation ||
            HubWire.Identity(value, "acquisition") != Acquisition) throw new HubException(HubFailure.Protocol);
    }
}

/// An immutable operation pin needs neither an ordinary output nor its client lease.
public sealed class HubGroupImageRequest : HubImageIdentity
{
    public Guid Group { get; }
    public Guid Operation { get; }
    public HubGroupImageRequest(Guid hostInstance, Guid configurationRevision, Guid group,
        Guid operation, Guid source, Guid generation, Guid acquisition)
        : base(hostInstance, configurationRevision, source, generation, acquisition)
    {
        if (group == Guid.Empty || operation == Guid.Empty) throw new HubException(HubFailure.InvalidRequest);
        Group = group; Operation = operation;
    }
    internal override string OperationKey => "cameraGroupImage";
    internal override object Wire() => new { hostInstance = HostInstance, configurationRevision = ConfigurationRevision,
        group = Group, operation = Operation, source = Source, generation = Generation, acquisition = Acquisition };
    internal override void Match(JsonElement value)
    {
        HubWire.Members(value, "hostInstance", "configurationRevision", "group", "operation", "source", "generation", "acquisition");
        if (HubWire.Identity(value, "hostInstance") != HostInstance || HubWire.Identity(value, "configurationRevision") != ConfigurationRevision ||
            HubWire.Identity(value, "group") != Group || HubWire.Identity(value, "operation") != Operation ||
            HubWire.Identity(value, "source") != Source || HubWire.Identity(value, "generation") != Generation ||
            HubWire.Identity(value, "acquisition") != Acquisition) throw new HubException(HubFailure.Protocol);
    }
}

/// Each handle owns a pin on immutable encoded pixels. Pin() shares storage;
/// CopyTo never exposes the backing array. Disposing one reader leaves others valid.
public sealed class HubCameraImage : IDisposable
{
    private readonly object gate = new();
    private Storage? storage;
    public HubImageIdentity Request { get; }
    public HubImageDescriptor Descriptor { get; }
    public int ByteLength => Descriptor.ByteLength;
    internal HubCameraImage(HubImageIdentity request, HubImageDescriptor descriptor, byte[] pixels, HubImageBudget.Reservation reservation)
    { Request = request; Descriptor = descriptor; storage = new Storage(pixels, reservation); }
    private HubCameraImage(HubImageIdentity request, HubImageDescriptor descriptor)
    { Request = request; Descriptor = descriptor; }
    public HubCameraImage Pin()
    {
        lock (gate) {
            var held = storage ?? throw new ObjectDisposedException(nameof(HubCameraImage));
            // Allocate the handle before acquiring its reference. A failed CLR
            // allocation must not strand a reference and its budget charge.
            var pin = new HubCameraImage(Request, Descriptor);
            Interlocked.Increment(ref held.Readers);
            pin.storage = held;
            return pin;
        }
    }
    /// Copies at most one transport chunk. Numeric bytes are little endian in
    /// ASCOM Array[X,Y,plane] order; no clamping or conversion is implicit.
    public void CopyTo(int sourceOffset, byte[] destination, int destinationOffset, int count)
    {
        if (destination is null) throw new ArgumentNullException(nameof(destination));
        if (count < 0 || count > HubCameraImages.ChunkBytes || sourceOffset < 0 || sourceOffset > ByteLength - count ||
            destinationOffset < 0 || destinationOffset > destination.Length - count) throw new ArgumentOutOfRangeException(nameof(count));
        lock (gate) {
            var held = storage ?? throw new ObjectDisposedException(nameof(HubCameraImage));
            Buffer.BlockCopy(held.Pixels, sourceOffset, destination, destinationOffset, count);
        }
    }
    public void Dispose()
    {
        Storage? held;
        lock (gate) { held = storage; storage = null; }
        held?.Release(); GC.SuppressFinalize(this);
    }
    // A field initializer can itself fail under memory pressure. Finalization
    // also runs for partially constructed objects, before gate exists.
    ~HubCameraImage() { if (gate is not null) Dispose(); }
    private sealed class Storage(byte[] pixels, HubImageBudget.Reservation reservation)
    {
        internal readonly byte[] Pixels = pixels;
        internal int Readers = 1;
        internal void Release()
        {
            if (Interlocked.Decrement(ref Readers) != 0) return;
            Array.Clear(Pixels, 0, Pixels.Length);
            reservation.Dispose();
        }
    }
}
