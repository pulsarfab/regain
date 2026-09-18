using System.ComponentModel.Composition;
using NINA.Core.Model.Equipment;
using NINA.Core.Utility;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.ViewModel;
using NINA.Profile.Interfaces;
using ZwoGain.Rotator;

namespace ZwoGain.NINA;

[Export(typeof(IEquipmentProvider))]
public sealed class EafProvider : IEquipmentProvider<IFocuser>
{
    public string Name => "ZWOgain";
    public IList<IFocuser> GetEquipment() => [new EafFocuser()];
}
[Export(typeof(IEquipmentProvider))]
public sealed class EfwProvider : IEquipmentProvider<IFilterWheel>
{
    private readonly IProfileService profiles;
    [ImportingConstructor] public EfwProvider(IProfileService profiles) => this.profiles = profiles;
    public string Name => "ZWOgain";
    public IList<IFilterWheel> GetEquipment() => [new EfwFilterWheel(profiles)];
}
public abstract class AccessoryDevice : BaseINPC, IDevice, IDisposable
{
    protected readonly AccessorySession Session;
    protected AccessoryDevice(string kind) => Session = new(Environment.GetEnvironmentVariable("ZWOGAIN_ACCESSORY_WORKER") ?? Path.Combine(CameraProvider.DirectoryPath, "zwogain-accessories.exe"), kind, AccessorySession.SettingsPath(kind, "nina"));
    public string Id => "ZwoGain." + Session.Kind.ToUpperInvariant();
    public string Name => Session.Kind == "efw" ? "ZWOgain EFW Filter Wheel" : "ZWOgain EAF Focuser";
    public string DisplayName => Name;
    public string Category => "ZWOgain";
    public string Description => Name + " over USB HID";
    public string DriverInfo => "ZWOgain native Rust USB driver";
    public string DriverVersion => "0.3.0";
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
    public IList<string> SupportedActions => Session.Kind == "efw" ? ["ZwoGain.Status", "ZwoGain.Identity", "ZwoGain.Calibrate"] : ["ZwoGain.Status", "ZwoGain.Identity"];
    public string Action(string actionName, string actionParameters) => actionName.ToLowerInvariant() switch {
        "zwogain.status" => Session.Request(new { command = "status" }).GetRawText(),
        "zwogain.identity" => Session.Request(new { command = "identity" }).GetRawText(),
        "zwogain.calibrate" when Session.Kind == "efw" => Calibrate(),
        _ => throw new NotSupportedException(actionName)
    };
    private string Calibrate() { Session.Calibrate(); RaiseAllPropertiesChanged(); return "null"; }
    public string SendCommandString(string command, bool raw = true) => throw new NotSupportedException();
    public bool SendCommandBool(string command, bool raw = true) => throw new NotSupportedException();
    public void SendCommandBlind(string command, bool raw = true) => throw new NotSupportedException();
    public void Dispose() => Session.Dispose();
}
public sealed class EafFocuser() : AccessoryDevice("eaf"), IFocuser
{
    public bool IsMoving => Session.Status().Moving;
    public int Position => Session.Status().Position;
    public int MaxStep => Session.Status().MaxStep;
    public int MaxIncrement => MaxStep;
    public double StepSize => double.NaN;
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
                    if (status.Position != position) throw new IOException("Focuser stopped before reaching the requested position");
                    if (waitInMs > 0) await Task.Delay(waitInMs, ct);
                    return;
                }
                await Task.Delay(100, ct);
            }
        } catch { try { Session.Halt(); } catch (Exception error) { Logger.Error(error.Message); } throw; }
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
