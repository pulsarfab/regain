using System.ComponentModel.Composition;
using System.Text.Json;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.ViewModel;
using Regain.Hub;

namespace Regain.NINA;

[Export(typeof(IEquipmentProvider))]
public sealed class HubCoverCalibratorProvider : IEquipmentProvider<IFlatDevice>
{
    public string Name => "PulsarFab regain";
    public IList<IFlatDevice> GetEquipment() => HubEquipment.Choices<IFlatDevice>("covercalibrator", binding => new HubCoverCalibratorDevice(binding));
}

public sealed class HubCoverCalibratorDevice : HubTypedDevice, IFlatDevice
{
    private readonly object brightnessGate = new();
    private int? requestedBrightness;
    private Guid requestedBrightnessEpoch;
    public HubCoverCalibratorDevice(HubSelection? selection,string? executable = null,string? workers = null)
        : base("covercalibrator",selection,executable,workers) { }
    private Task<JsonElement> Read(Guid epoch,Guid output,HubCoverCalibratorProperty property,CancellationToken token)
        => ReadTyped(epoch,output,HubCoverCalibratorProtocol.Read(property),value => HubCoverCalibratorProtocol.Validate(property,value),token);
    private JsonElement Read(HubCoverCalibratorProperty property)
    {
        var context = RequireContext();
        return Read(context.Epoch,context.Binding.OutputId,property,CancellationToken.None).GetAwaiter().GetResult();
    }
    protected override async Task<Action> Prepare(Guid epoch,Guid output,CancellationToken token)
    {
        var state = (await Read(epoch,output,HubCoverCalibratorProperty.CalibratorState,token).ConfigureAwait(false)).GetInt32();
        int? brightness = state is 2 or 3 ? (await Read(epoch,output,HubCoverCalibratorProperty.Brightness,token).ConfigureAwait(false)).GetInt32() : null;
        return () => { lock (brightnessGate) { requestedBrightness = brightness; requestedBrightnessEpoch = epoch; } };
    }
    public CoverState CoverState => Read(HubCoverCalibratorProperty.CoverState).GetInt32() switch {
        0 => CoverState.NotPresent, 1 => CoverState.Closed, 2 => CoverState.NeitherOpenNorClosed,
        3 => CoverState.Open, 4 => CoverState.Unknown, 5 => CoverState.Error,
        _ => throw new InvalidDataException("Hub returned an invalid panel state")
    };
    public bool CoverMoving => Read(HubCoverCalibratorProperty.CoverMoving).GetBoolean();
    public bool CalibratorChanging => Read(HubCoverCalibratorProperty.CalibratorChanging).GetBoolean();
    public bool SupportsOpenClose => ReadContext is not null && Read(HubCoverCalibratorProperty.CoverState).GetInt32() != 0;
    public bool SupportsOnOff => ReadContext is not null && Read(HubCoverCalibratorProperty.CalibratorState).GetInt32() != 0;
    public int MinBrightness => 0;
    public int MaxBrightness => SupportsOnOff ? Read(HubCoverCalibratorProperty.MaxBrightness).GetInt32() : 0;
    public int Brightness {
        get => SupportsOnOff ? Read(HubCoverCalibratorProperty.Brightness).GetInt32() : 0;
        set => SetLight(value).GetAwaiter().GetResult();
    }
    public bool LightOn {
        get {
            if (ReadContext is null) return false;
            var state = Read(HubCoverCalibratorProperty.CalibratorState).GetInt32();
            if (state is 4 or 5) { Failed(); throw new InvalidOperationException("Panel illumination state is unavailable; inspect source health"); }
            // Logical On(0) remains on; zero brightness is not an Off command.
            return state is 2 or 3;
        }
        set {
            if (!value) { SetLight(null).GetAwaiter().GetResult(); return; }
            var context = RequireContext();
            int? brightness; lock (brightnessGate) brightness = requestedBrightnessEpoch == context.Epoch ? requestedBrightness : null;
            var level = brightness ?? Read(context.Epoch,context.Binding.OutputId,HubCoverCalibratorProperty.MaxBrightness,CancellationToken.None).GetAwaiter().GetResult().GetInt32();
            SetLight(level,context).GetAwaiter().GetResult();
        }
    }
    public string PortName { get => ""; set { } } // Port selection belongs to shared source setup.
    private async Task SetLight(int? brightness,(HubSelection Binding,Guid Epoch)? expected = null)
    {
        // Validate before any dispatch. The Rust controller checks live limits.
        var command = brightness is { } level ? HubCoverCalibratorProtocol.On(level) : HubCoverCalibratorProtocol.Off();
        var context = expected ?? RequireContext();
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(300));
        try {
            await WriteTyped(context.Epoch,context.Binding.OutputId,command,deadline.Token).ConfigureAwait(false);
            while ((await Read(context.Epoch,context.Binding.OutputId,HubCoverCalibratorProperty.CalibratorChanging,deadline.Token).ConfigureAwait(false)).GetBoolean()) {
                await Task.Delay(100,deadline.Token).ConfigureAwait(false);
            }
            var state = (await Read(context.Epoch,context.Binding.OutputId,HubCoverCalibratorProperty.CalibratorState,deadline.Token).ConfigureAwait(false)).GetInt32();
            var actual = (await Read(context.Epoch,context.Binding.OutputId,HubCoverCalibratorProperty.Brightness,deadline.Token).ConfigureAwait(false)).GetInt32();
            if (state != (brightness is null ? 1 : 3) || actual != (brightness ?? 0))
                throw new IOException("Panel did not reach the requested illumination state; inspect the source before another command");
            if (brightness is not null) {
                lock (brightnessGate) { if (requestedBrightnessEpoch == context.Epoch) requestedBrightness = brightness; }
            }
            RaiseAllPropertiesChanged();
        } catch (OperationCanceledException error) when (deadline.IsCancellationRequested) {
            Failed(); throw new TimeoutException("Panel illumination completion timed out; inspect the source before another command",error);
        } catch { Failed(); throw; }
    }
    private async Task<bool> MoveCover(bool open,CancellationToken token,int delay)
    {
        if (delay is < 1 or > 10000) throw new ArgumentOutOfRangeException(nameof(delay));
        token.ThrowIfCancellationRequested();
        var context = RequireContext();
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(token);
        deadline.CancelAfter(TimeSpan.FromSeconds(300));
        try {
            await WriteTyped(context.Epoch,context.Binding.OutputId,open ? HubCoverCalibratorProtocol.Open() : HubCoverCalibratorProtocol.Close(),deadline.Token).ConfigureAwait(false);
            while ((await Read(context.Epoch,context.Binding.OutputId,HubCoverCalibratorProperty.CoverMoving,deadline.Token).ConfigureAwait(false)).GetBoolean()) {
                RaiseAllPropertiesChanged(); await Task.Delay(delay,deadline.Token).ConfigureAwait(false);
            }
            var actual = (await Read(context.Epoch,context.Binding.OutputId,HubCoverCalibratorProperty.CoverState,deadline.Token).ConfigureAwait(false)).GetInt32();
            if (actual != (open ? 3 : 1)) throw new IOException("Panel cover stopped without reaching the requested endpoint; inspect the source before another command");
            RaiseAllPropertiesChanged(); return true;
        } catch (OperationCanceledException error) when (!token.IsCancellationRequested && deadline.IsCancellationRequested) {
            Failed(); throw new TimeoutException("Panel cover completion timed out; inspect the source before another command",error);
        } catch { Failed(); throw; }
        // Cancellation never halts another client's later cover movement.
    }
    public Task<bool> Open(CancellationToken ct,int delay = 300) => MoveCover(true,ct,delay);
    public Task<bool> Close(CancellationToken ct,int delay = 300) => MoveCover(false,ct,delay);
    public void HaltCover()
    {
        var context = RequireContext();
        WriteTyped(context.Epoch,context.Binding.OutputId,HubCoverCalibratorProtocol.Halt(),CancellationToken.None).GetAwaiter().GetResult();
        RaiseAllPropertiesChanged();
    }
}
