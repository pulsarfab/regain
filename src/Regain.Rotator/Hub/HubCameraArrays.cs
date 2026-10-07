using System.Runtime.CompilerServices;

namespace Regain.Hub;

/// Lossless CLR arrays for COM/NINA consumers. Reservations follow returned
/// arrays through GC, including arrays retained after their driver is disposed.
public static class HubCameraArrays
{
    private static readonly ConditionalWeakTable<Array, Retained> retained = new();
    /// Adapt an immutable ASCOM-order scalar frame to row-major UInt16/Int32.
    /// Every pixel must be exactly representable; never round or drop channels.
    /// The returned array retains its own reservation after the encoded image dies.
    public static Array RowMajorIntegers(HubCameraImage image, bool signed32 = false,
        HubImageBudget? budget = null, CancellationToken cancellation = default)
    {
        cancellation.ThrowIfCancellationRequested();
        using var pin = image.Pin();
        var descriptor = pin.Descriptor;
        if (descriptor.Planes is > 1)
            throw new NotSupportedException("The scalar image pipeline cannot represent multiple color planes");
        if (!BitConverter.IsLittleEndian) throw new HubException(HubFailure.Protocol);
        budget ??= HubImageBudget.Shared;
        var count = checked((long)descriptor.Width * descriptor.Height);
        var bytes = checked(count * (signed32 ? 4 : 2) + 256);
        if (bytes > HubImageBudget.MaximumImageBytes) throw new HubException(HubFailure.Busy);
        Retained? charge = null;
        try {
            charge = Retained.Create(budget, (int)bytes);
            var length = Math.Min(pin.ByteLength, HubCameraImages.ChunkBytes);
            using var scratch = budget.Reserve(length);
            var buffer = new byte[length];
            Array result = signed32 ? new int[(int)count] : new ushort[(int)count];
            var size = HubImageDescriptor.Size(descriptor.TransmissionType);
            for (var offset = 0; offset < pin.ByteLength; offset += length) {
                cancellation.ThrowIfCancellationRequested();
                var used = Math.Min(length, pin.ByteLength - offset);
                pin.CopyTo(offset, buffer, 0, used);
                for (var start = 0; start < used; start += size) {
                    // All integers in either destination range are exactly
                    // representable as Double. Values outside the range are
                    // rejected before conversion, including Int64/UInt64 inputs.
                    var value = Number(buffer, start, descriptor.TransmissionType);
                    if (double.IsNaN(value) || double.IsInfinity(value) || value != Math.Truncate(value)
                        || value < (signed32 ? int.MinValue : 0) || value > (signed32 ? int.MaxValue : ushort.MaxValue))
                        throw new NotSupportedException("A pixel cannot be represented exactly by the selected integer image format");
                    var element = (offset + start) / size;
                    var destination = (element % descriptor.Height) * descriptor.Width + element / descriptor.Height;
                    if (signed32) ((int[])result)[destination] = (int)value;
                    else ((ushort[])result)[destination] = (ushort)value;
                }
            }
            cancellation.ThrowIfCancellationRequested();
            retained.Add(result, charge); charge = null;
            return result;
        } catch (OutOfMemoryException) { throw new HubException(HubFailure.Busy); }
        finally { charge?.Dispose(); }
    }
    private static double Number(byte[] bytes, int offset, HubImageElementType kind) => kind switch {
        HubImageElementType.Byte => bytes[offset], HubImageElementType.Int16 => BitConverter.ToInt16(bytes, offset),
        HubImageElementType.Int32 => BitConverter.ToInt32(bytes, offset), HubImageElementType.Int64 => BitConverter.ToInt64(bytes, offset),
        HubImageElementType.UInt16 => BitConverter.ToUInt16(bytes, offset), HubImageElementType.UInt32 => BitConverter.ToUInt32(bytes, offset),
        HubImageElementType.UInt64 => BitConverter.ToUInt64(bytes, offset), HubImageElementType.Single => BitConverter.ToSingle(bytes, offset),
        HubImageElementType.Double => BitConverter.ToDouble(bytes, offset), _ => throw new HubException(HubFailure.Protocol)
    };
    public static Array Convert(HubCameraImage image, bool variants = false, HubImageBudget? budget = null,
        CancellationToken cancellation = default)
    {
        cancellation.ThrowIfCancellationRequested();
        using var pin = image.Pin();
        var descriptor = pin.Descriptor;
        if (!BitConverter.IsLittleEndian) throw new HubException(HubFailure.Protocol);
        budget ??= HubImageBudget.Shared;
        var count = checked((long)descriptor.Width * descriptor.Height * (descriptor.Planes ?? 1));
        // Boxed primitives occupy at most 24 bytes on our supported x86/x64
        // runtimes; charge pointers too. Array header/alignment is conservative.
        var bytes = count * (variants ? IntPtr.Size + 24 : HubImageDescriptor.Size(descriptor.ElementType)) + 256;
        if (bytes > HubImageBudget.MaximumImageBytes) throw new HubException(HubFailure.Busy);
        var charge = Retained.Create(budget, (int)bytes);
        try {
            var length = Math.Min(pin.ByteLength, HubCameraImages.ChunkBytes);
            using var scratch = budget.Reserve(length);
            var buffer = new byte[length];
            var dimensions = descriptor.Planes.HasValue ? new[] { descriptor.Width, descriptor.Height, descriptor.Planes.Value }
                : new[] { descriptor.Width, descriptor.Height };
            var result = Array.CreateInstance(variants ? typeof(object) : Type(descriptor.ElementType), dimensions);
            var size = HubImageDescriptor.Size(descriptor.TransmissionType);
            for (var offset = 0; offset < pin.ByteLength; offset += length) {
                cancellation.ThrowIfCancellationRequested();
                var used = Math.Min(length, pin.ByteLength - offset);
                pin.CopyTo(offset, buffer, 0, used);
                if (!variants && descriptor.ElementType == descriptor.TransmissionType) {
                    Buffer.BlockCopy(buffer, 0, result, offset, used); continue;
                }
                for (var start = 0; start < used; start += size) {
                    object value = Value(buffer, start, descriptor.TransmissionType);
                    if (descriptor.ElementType == HubImageElementType.Int32) value = System.Convert.ToInt32(value);
                    var element = (offset + start) / size;
                    var planes = descriptor.Planes ?? 1;
                    var x = element / (descriptor.Height * planes); var y = element / planes % descriptor.Height;
                    if (descriptor.Planes.HasValue) result.SetValue(value, x, y, element % planes);
                    else result.SetValue(value, x, y);
                }
            }
            cancellation.ThrowIfCancellationRequested();
            retained.Add(result, charge); charge = null!;
            return result;
        } catch (OutOfMemoryException) { throw new HubException(HubFailure.Busy); }
        finally { charge?.Dispose(); }
    }
    private static Type Type(HubImageElementType kind) => kind switch {
        HubImageElementType.Byte => typeof(byte), HubImageElementType.Int16 => typeof(short),
        HubImageElementType.Int32 => typeof(int), HubImageElementType.Int64 => typeof(long),
        HubImageElementType.UInt16 => typeof(ushort), HubImageElementType.UInt32 => typeof(uint),
        HubImageElementType.UInt64 => typeof(ulong), HubImageElementType.Single => typeof(float),
        HubImageElementType.Double => typeof(double), _ => throw new HubException(HubFailure.Protocol)
    };
    private static object Value(byte[] bytes, int offset, HubImageElementType kind) => kind switch {
        HubImageElementType.Byte => (object)bytes[offset], HubImageElementType.Int16 => (object)BitConverter.ToInt16(bytes, offset),
        HubImageElementType.Int32 => (object)BitConverter.ToInt32(bytes, offset), HubImageElementType.Int64 => (object)BitConverter.ToInt64(bytes, offset),
        HubImageElementType.UInt16 => (object)BitConverter.ToUInt16(bytes, offset), HubImageElementType.UInt32 => (object)BitConverter.ToUInt32(bytes, offset),
        HubImageElementType.UInt64 => (object)BitConverter.ToUInt64(bytes, offset), HubImageElementType.Single => (object)BitConverter.ToSingle(bytes, offset),
        HubImageElementType.Double => (object)BitConverter.ToDouble(bytes, offset), _ => throw new HubException(HubFailure.Protocol)
    };
    private sealed class Retained(HubImageBudget.Reservation reservation) : IDisposable {
        private HubImageBudget.Reservation? charge = reservation;
        internal static Retained Create(HubImageBudget budget, int count) {
            var reservation = budget.Reserve(count);
            try { return new Retained(reservation); }
            catch { reservation.Dispose(); throw; }
        }
        public void Dispose() { Interlocked.Exchange(ref charge, null)?.Dispose(); GC.SuppressFinalize(this); }
        ~Retained() { Dispose(); }
    }
}
