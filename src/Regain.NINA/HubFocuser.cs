using System.ComponentModel.Composition;
using System.Text.Json;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.ViewModel;
using Regain.Hub;

namespace Regain.NINA;

[Export(typeof(IEquipmentProvider))]
public sealed class HubFocuserProvider : IEquipmentProvider<IFocuser>
{
    public string Name => "PulsarFab regain";
    public IList<IFocuser> GetEquipment() => HubEquipment.Choices<IFocuser>("focuser", binding => new HubFocuserDevice(binding));
}

public sealed class HubFocuserDevice : HubTypedDevice, IFocuser
{
    public HubFocuserDevice(HubSelection? selection, string? executable = null, string? workers = null) : base("focuser", selection, executable, workers) { }
    protected override async Task<Action> Prepare(Guid epoch, Guid output, CancellationToken token)
    {
        // NINA's focuser interface requires an absolute Position. Relative
        // sources remain available through ASCOM/Alpaca without invented data.
        if (!(await Read(epoch, output, HubFocuserProperty.Absolute, token).ConfigureAwait(false)).GetBoolean())
            throw new NotSupportedException("NINA requires an absolute focuser; use the relative output through Alpaca or ASCOM");
        return () => { };
    }
    private Task<JsonElement> Read(Guid epoch, Guid output, HubFocuserProperty property, CancellationToken token)
        => ReadTyped(epoch, output, HubFocuserProtocol.Read(property), value => HubFocuserProtocol.Validate(property, value), token);
    private JsonElement Read(HubFocuserProperty property)
    {
        var context = RequireContext();
        return Read(context.Epoch, context.Binding.OutputId, property, CancellationToken.None).GetAwaiter().GetResult();
    }
    public bool IsMoving => Read(HubFocuserProperty.IsMoving).GetBoolean();
    public int Position => Read(HubFocuserProperty.Position).GetInt32();
    public int MaxStep => Read(HubFocuserProperty.MaxStep).GetInt32();
    public int MaxIncrement => Read(HubFocuserProperty.MaxIncrement).GetInt32();
    public bool TempCompAvailable => Read(HubFocuserProperty.TempCompAvailable).GetBoolean();
    public bool TempComp {
        get => Read(HubFocuserProperty.TempComp).GetBoolean();
        set {
            var context = RequireContext();
            Write(context.Epoch, context.Binding.OutputId, HubFocuserProtocol.TempComp(value), CancellationToken.None).GetAwaiter().GetResult();
        }
    }
    private double Optional(HubFocuserProperty property)
    {
        try { return Read(property).GetDouble(); }
        catch (HubException error) when (error.Remote?.Code == "unsupported") { return double.NaN; }
    }
    public double Temperature => Optional(HubFocuserProperty.Temperature);
    public double StepSize => Optional(HubFocuserProperty.StepSize);
    private async Task Write(Guid epoch, Guid output, object property, CancellationToken token)
        => await WriteTyped(epoch, output, property, token).ConfigureAwait(false);
    public async Task Move(int position, CancellationToken token, int waitInMs = 1000)
    {
        if (waitInMs < 0) throw new ArgumentOutOfRangeException(nameof(waitInMs));
        token.ThrowIfCancellationRequested();
        var context = RequireContext();
        await Write(context.Epoch, context.Binding.OutputId, HubFocuserProtocol.Move(position), token).ConfigureAwait(false);
        while ((await Read(context.Epoch, context.Binding.OutputId, HubFocuserProperty.IsMoving, token).ConfigureAwait(false)).GetBoolean()) {
            RaiseAllPropertiesChanged();
            await Task.Delay(100, token).ConfigureAwait(false);
        }
        if ((await Read(context.Epoch, context.Binding.OutputId, HubFocuserProperty.Position, token).ConfigureAwait(false)).GetInt32() != position)
            throw new IOException("Focuser stopped before reaching the requested position; inspect source health");
        if (waitInMs > 0) await Task.Delay(waitInMs, token).ConfigureAwait(false);
        RaiseAllPropertiesChanged();
        // Cancellation does not imply Halt: another client may own subsequent
        // motion. A caller must explicitly reconcile and request Halt if needed.
    }
    public void Halt()
    {
        var context = RequireContext();
        Write(context.Epoch, context.Binding.OutputId, HubFocuserProtocol.Halt(), CancellationToken.None).GetAwaiter().GetResult();
        RaiseAllPropertiesChanged();
    }
}
