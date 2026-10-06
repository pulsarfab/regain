using System.Text.Json;
using Regain.Hub;

namespace Regain.NINA;

/// Shared native request/failure handling for typed accessories. Each device
/// retains its own capabilities, coordinate rules and completion semantics.
public abstract class HubTypedDevice : HubDevice
{
    protected HubTypedDevice(string type, HubSelection? selection, string? executable, string? workers)
        : base(type, selection, executable, workers) { }
    protected (HubSelection Binding, Guid Epoch) RequireContext() => ReadContext
        ?? throw new InvalidOperationException("Connect this hub accessory explicitly before reading or moving");
    protected async Task<JsonElement> ReadTyped(Guid epoch, Guid output, object property,
        Func<JsonElement, JsonElement> validate, CancellationToken token)
    {
        try {
            var value = await Session.RequestAsync(epoch, JsonSerializer.SerializeToElement(new { op = "get", output, property }),
                cancellation: token).ConfigureAwait(false);
            return validate(value);
        } catch (HubException error) when (error.Remote?.Code == "unsupported") { throw; }
        catch { Failed(); throw; }
    }
    protected async Task<JsonElement> WriteTyped(Guid epoch, Guid output, object property, CancellationToken token)
    {
        try {
            return await Session.RequestAsync(epoch, JsonSerializer.SerializeToElement(new { op = "put", output, property }),
                TimeSpan.FromSeconds(35), token).ConfigureAwait(false);
        } catch { Failed(); throw; }
    }
    public override bool Connected {
        get {
            if (ReadContext is not { } context) return false;
            try { return Get(context.Epoch, context.Binding.OutputId, new { member = "connected" }).GetBoolean(); }
            catch { Failed(); return false; }
        }
    }
}
