using System.Runtime.CompilerServices;

namespace Regain.Hub;

/// Lossless CLR arrays for COM/NINA consumers. Reservations follow returned
/// arrays through GC, including arrays retained after their driver is disposed.
public static class HubCameraArrays
{
    private static readonly ConditionalWeakTable<Array, Retained> retained = new();
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
