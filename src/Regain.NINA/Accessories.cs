using System.ComponentModel.Composition;
using NINA.Core.Model.Equipment;
using NINA.Core.Utility;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.ViewModel;
using NINA.Profile.Interfaces;
using Regain.Rotator;

namespace Regain.NINA;

[Export(typeof(IEquipmentProvider))]
public sealed class EtaProvider : IEquipmentProvider<IFocuser>
{
    public string Name => "PulsarFab regain";
    public IList<IFocuser> GetEquipment() => [new EtaFocuser()];
}
[Export(typeof(IEquipmentProvider))]
public sealed class FocusCubeProvider : IEquipmentProvider<IFocuser>
{
    public string Name => "PulsarFab regain";
    public IList<IFocuser> GetEquipment() => [new FocusCubeFocuser()];
}
[Export(typeof(IEquipmentProvider))]
public sealed class EafProvider : IEquipmentProvider<IFocuser>
{
    public string Name => "PulsarFab regain";
    public IList<IFocuser> GetEquipment() => [new EafFocuser()];
}
[Export(typeof(IEquipmentProvider))]
public sealed class EfwProvider : IEquipmentProvider<IFilterWheel>
{
    private readonly IProfileService profiles;
    [ImportingConstructor] public EfwProvider(IProfileService profiles) => this.profiles = profiles;
    public string Name => "PulsarFab regain";
    public IList<IFilterWheel> GetEquipment() => [new EfwFilterWheel(profiles)];
}
public abstract class AccessoryDevice : BaseINPC, IDevice, IDisposable
{
    protected readonly AccessorySession Session;
    protected AccessoryDevice(string kind) => Session = new(Regain.Rotator.RegainPaths.EnvironmentVariable(kind == "eta" ? "REGAIN_ETA_WORKER" : kind == "fc3" ? "REGAIN_FC3_WORKER" : "REGAIN_ACCESSORY_WORKER") ?? Path.Combine(CameraProvider.DirectoryPath, kind == "eta" ? "regain-eta.exe" : kind == "fc3" ? "regain-fc3.exe" : "regain-accessories.exe"), kind, AccessorySession.SettingsPath(kind, "nina"));
    public string Id => (Session.Kind == "eta" ? "Regain." : "ZwoGain.") + Session.Kind.ToUpperInvariant();
    public string Name => Session.Kind == "eta" ? "PulsarFab regain Wanderer Astro ETA M54" : Session.Kind == "efw" ? "PulsarFab regain EFW Filter Wheel" : Session.Kind == "fc3" ? "PulsarFab regain Pegasus FocusCube3" : "PulsarFab regain EAF Focuser";
    public string DisplayName => Name;
    public string Category => "PulsarFab regain";
    public string Description => Name + (Session.Kind is "fc3" or "eta" ? " over USB serial" : " over USB HID");
    public string DriverInfo => "PulsarFab regain native Rust USB driver";
    public string DriverVersion => typeof(AccessoryDevice).Assembly.GetName().Version!.ToString();
    public bool HasSetupDialog => true;
    public bool Connected => Session.Connected;
    public virtual async Task<bool> Connect(CancellationToken token)
    {
        token.ThrowIfCancellationRequested(); await Task.Run(Session.Connect, token);
        if (token.IsCancellationRequested) { Session.Disconnect(); token.ThrowIfCancellationRequested(); }
        RaiseAllPropertiesChanged(); return true;
    }
    public void Disconnect() { try { Session.Disconnect(); } finally { RaiseAllPropertiesChanged(); } }
    public void SetupDialog() { AccessorySetupWindow.Show(Session); RaiseAllPropertiesChanged(); }
    public IList<string> SupportedActions => Session.Kind == "eta" ? ["Regain.Status", "Regain.Identity", "Regain.MovePoint", "Regain.CancelQueued"] : Session.Kind == "efw" ? ["Regain.Status", "Regain.Identity", "Regain.Calibrate"] : ["Regain.Status", "Regain.Identity"];
    public string Action(string actionName, string actionParameters) => RegainPaths.ActionName(actionName) switch {
        "regain.status" => Session.Request(new { command = "status" }).GetRawText(),
        "regain.identity" => Session.Request(new { command = "identity" }).GetRawText(),
        "regain.calibrate" when Session.Kind == "efw" => Calibrate(),
        "regain.movepoint" when Session.Kind == "eta" => Session.EtaMovePoint(actionParameters),
        "regain.cancelqueued" when Session.Kind == "eta" => Session.Request(new { command = "cancel-queued" }).GetRawText(),
        _ => throw new NotSupportedException(actionName)
    };
    private string Calibrate() { Session.Calibrate(); RaiseAllPropertiesChanged(); return "null"; }
    public string SendCommandString(string command, bool raw = true) => throw new NotSupportedException();
    public bool SendCommandBool(string command, bool raw = true) => throw new NotSupportedException();
    public void SendCommandBlind(string command, bool raw = true) => throw new NotSupportedException();
    public void Dispose() => Session.Dispose();
}
public sealed class EtaFocuser() : NativeFocuser("eta");
public sealed class EafFocuser() : NativeFocuser("eaf");
public sealed class FocusCubeFocuser() : NativeFocuser("fc3");
public abstract class NativeFocuser(string kind) : AccessoryDevice(kind), IFocuser
{
    public bool IsMoving => Session.Status().Moving;
    public int Position => Session.Status().Position;
    public int MaxStep => Session.Status().MaxStep;
    public int MaxIncrement => MaxStep;
    public double StepSize => Session.Kind == "eta" ? 1.0 : double.NaN;
    public bool TempCompAvailable => false;
    public bool TempComp { get => false; set { if (value) throw new NotSupportedException("Use NINA temperature compensation"); } }
    public double Temperature => Session.Status().Temperature ?? double.NaN;
    public async Task Move(int position, CancellationToken ct, int waitInMs = 1000)
    {
        ct.ThrowIfCancellationRequested(); await Task.Run(() => Session.Move(position), ct);
        try {
            while (true) {
                ct.ThrowIfCancellationRequested();
                var status = await Task.Run(Session.Status, ct); RaiseAllPropertiesChanged();
                if (!status.Moving) {
                    if (Math.Abs(status.Position - position) > (Session.Kind == "eta" ? 2 : 0)) throw new IOException("Focuser stopped before reaching the requested position");
                    if (waitInMs > 0) await Task.Delay(waitInMs, ct);
                    return;
                }
                await Task.Delay(100, ct);
            }
        } catch { try { if (Session.Kind == "eta") Session.Request(new { command = "cancel-queued" }); else Session.Halt(); } catch (Exception error) { Logger.Error(error.Message); } throw; }
    }
    public void Halt() { Session.Halt(); RaiseAllPropertiesChanged(); }
}
public sealed class EfwFilterWheel(IProfileService profiles) : AccessoryDevice("efw"), IFilterWheel
{
    public int[] FocusOffsets => (int[])Session.Profile.FocusOffsets.Clone();
    public string[] Names => (string[])Session.Profile.Names.Clone();
    public short Position { get { var s = Session.Status(); return s.Moving ? (short)-1 : checked((short)s.Position); } set { Session.Move(value); RaiseAllPropertiesChanged(); } }
    public AsyncObservableCollection<FilterInfo> Filters => profiles.ActiveProfile.FilterWheelSettings.FilterWheelFilters;
    public override async Task<bool> Connect(CancellationToken token)
    {
        await base.Connect(token);
        // Preserve NINA's exposure/autofocus configuration for existing slots.
        for (int i = Filters.Count; i < Names.Length; i++) Filters.Add(new FilterInfo(Names[i], FocusOffsets[i], (short)i));
        while (Filters.Count > Names.Length) Filters.RemoveAt(Filters.Count - 1);
        RaiseAllPropertiesChanged(); return true;
    }
}
