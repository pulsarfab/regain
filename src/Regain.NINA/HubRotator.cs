using System.ComponentModel.Composition;
using System.Text.Json;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.ViewModel;
using Regain.Hub;

namespace Regain.NINA;

[Export(typeof(IEquipmentProvider))]
public sealed class HubRotatorProvider : IEquipmentProvider<IRotator>
{
    public string Name => "PulsarFab regain";
    public IList<IRotator> GetEquipment() => HubEquipment.Choices<IRotator>("rotator", binding => new HubRotatorDevice(binding));
}

public sealed class HubRotatorDevice : HubTypedDevice, IRotator
{
    private readonly object syncGate = new();
    private Guid syncedEpoch;
    public HubRotatorDevice(HubSelection? selection, string? executable = null, string? workers = null) : base("rotator", selection, executable, workers) { }
    protected override Task<Action> Prepare(Guid epoch, Guid output, CancellationToken token)
    {
        return Task.FromResult<Action>(() => { lock (syncGate) syncedEpoch = Guid.Empty; });
    }
    private Task<JsonElement> Read(Guid epoch, Guid output, HubRotatorProperty property, CancellationToken token)
        => ReadTyped(epoch, output, HubRotatorProtocol.Read(property), value => HubRotatorProtocol.Validate(property, value), token);
    private JsonElement Read(HubRotatorProperty property)
    {
        var context = RequireContext();
        return Read(context.Epoch, context.Binding.OutputId, property, CancellationToken.None).GetAwaiter().GetResult();
    }
    public bool Synced {
        get {
            var context = ReadContext;
            if (context is null || !Connected) return false;
            lock (syncGate) return context.Value.Epoch == syncedEpoch;
        }
    }
    public bool CanReverse => Read(HubRotatorProperty.CanReverse).GetBoolean();
    public bool Reverse {
        get => Read(HubRotatorProperty.Reverse).GetBoolean();
        set {
            var context = RequireContext();
            Write(context.Epoch, context.Binding.OutputId, HubRotatorProtocol.Reverse(value), CancellationToken.None).GetAwaiter().GetResult();
            RaiseAllPropertiesChanged();
        }
    }
    public bool IsMoving => Read(HubRotatorProperty.IsMoving).GetBoolean();
    public float Position => Read(HubRotatorProperty.Position).GetSingle();
    public float MechanicalPosition => Read(HubRotatorProperty.MechanicalPosition).GetSingle();
    public float StepSize {
        get {
            try { return Read(HubRotatorProperty.StepSize).GetSingle(); }
            catch (HubException error) when (error.Remote?.Code == "unsupported") { return float.NaN; }
        }
    }
    private Task<JsonElement> Write(Guid epoch, Guid output, object property, CancellationToken token)
        => WriteTyped(epoch, output, property, token);
    public void Sync(float skyAngle)
    {
        HubRotatorProtocol.ValidateCommand(skyAngle, true);
        var context = RequireContext();
        lock (syncGate) syncedEpoch = Guid.Empty;
        Write(context.Epoch, context.Binding.OutputId, HubRotatorProtocol.Sync(skyAngle), CancellationToken.None).GetAwaiter().GetResult();
        // Per-connection NINA indication, not a second offset or a claim about
        // another client's calibration. All clients read the source's mapping.
        lock (syncGate) syncedEpoch = context.Epoch;
        RaiseAllPropertiesChanged();
    }
    private async Task<bool> Move(object command, float? expected, bool mechanical, CancellationToken token)
    {
        token.ThrowIfCancellationRequested();
        var context = RequireContext();
        try {
        var receipt = await Write(context.Epoch, context.Binding.OutputId, command, token).ConfigureAwait(false);
        var target = expected;
        (float Expected, float Accepted)? relative = expected is null ? HubRotatorProtocol.TargetReceipt(receipt) : null;
        float step;
        try { step = (await Read(context.Epoch, context.Binding.OutputId, HubRotatorProperty.StepSize, token).ConfigureAwait(false)).GetSingle(); }
        catch (HubException error) when (error.Remote?.Code == "unsupported") { step = float.NaN; }
        var tolerance = float.IsNaN(step) ? 0.01f : Math.Max(0.01f, step / 2);
        if (relative is { } accepted) {
            if (HubRotatorProtocol.AngularError(accepted.Expected, accepted.Accepted) > tolerance)
                throw new IOException("Rotator acknowledged the move without accepting its expected target; do not replay");
            target = accepted.Expected;
        }
        while ((await Read(context.Epoch, context.Binding.OutputId, HubRotatorProperty.IsMoving, token).ConfigureAwait(false)).GetBoolean()) {
            RaiseAllPropertiesChanged();
            await Task.Delay(100, token).ConfigureAwait(false);
        }
        var actual = (await Read(context.Epoch, context.Binding.OutputId, mechanical ? HubRotatorProperty.MechanicalPosition
            : HubRotatorProperty.Position, token).ConfigureAwait(false)).GetSingle();
        if (HubRotatorProtocol.AngularError(actual, target!.Value) > tolerance)
            throw new IOException("Rotator stopped before reaching the accepted target; inspect source health");
        RaiseAllPropertiesChanged();
        // Cancellation or failed readback cannot Halt a sibling's later motion.
        return true;
        } catch (OperationCanceledException) { throw; }
        catch { Failed(); throw; }
    }
    public Task<bool> Move(float position, CancellationToken ct)
    {
        HubRotatorProtocol.ValidateCommand(position, false);
        return Move(HubRotatorProtocol.MoveTracked(position), null, false, ct);
    }
    public Task<bool> MoveAbsolute(float position, CancellationToken ct)
    {
        HubRotatorProtocol.ValidateCommand(position, true);
        return Move(HubRotatorProtocol.MoveAbsolute(position), position, false, ct);
    }
    public Task<bool> MoveAbsoluteMechanical(float position, CancellationToken ct)
    {
        HubRotatorProtocol.ValidateCommand(position, true);
        return Move(HubRotatorProtocol.MoveMechanical(position), position, true, ct);
    }
    public void Halt()
    {
        var context = RequireContext();
        Write(context.Epoch, context.Binding.OutputId, HubRotatorProtocol.Halt(), CancellationToken.None).GetAwaiter().GetResult();
        RaiseAllPropertiesChanged();
    }
}
