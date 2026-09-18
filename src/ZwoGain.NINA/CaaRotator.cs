using System.ComponentModel.Composition;
using NINA.Core.Utility;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.ViewModel;
using ZwoGain.Rotator;

namespace ZwoGain.NINA;

[Export(typeof(IEquipmentProvider))]
public sealed class CaaProvider : IEquipmentProvider<IRotator>
{
    public string Name => "ZWOgain";
    public IList<IRotator> GetEquipment() => [new CaaRotator()];
}

public sealed class CaaRotator : BaseINPC, IRotator, IDisposable
{
    private static readonly string Worker = Path.Combine(CameraProvider.DirectoryPath, "zwogain-caa.exe");
    private CaaSession session = NewSession();
    private static CaaSession NewSession() => new(Worker, CaaSession.SettingsPath("nina")) { Log = message => Logger.Info("ZWOgain CAA: " + message) };
    public string Id => "ZwoGain.CAA";
    public string Name => "ZWOgain CAA Rotator";
    public string DisplayName => Name;
    public string Category => "ZWOgain";
    public string Description => "ZWO CAA rotator over USB HID";
    public string DriverInfo => "ZWOgain native Rust CAA driver";
    public string DriverVersion => typeof(CaaRotator).Assembly.GetName().Version!.ToString();
    public bool HasSetupDialog => true;
    public bool Connected => session.Connected;
    public bool CanReverse => true;
    public bool Reverse {
        get => session.Request(new { command = "settings" }).GetProperty("reverse").GetBoolean();
        set { session.Request(new { command = "reverse", enabled = value }); session.RememberCoordinates(); RaiseAllPropertiesChanged(); }
    }
    public bool IsMoving { get { var s = session.Status(); session.CheckMotion(s); return s.Moving; } }
    public bool Synced => session.Profile.Synced;
    public float Position => (float)session.Status().Logical;
    public float MechanicalPosition => (float)CaaSession.Wrap(session.Status().Mechanical);
    public float StepSize => 0.02f;
    public void Sync(float skyAngle) { session.Sync(skyAngle); RaiseAllPropertiesChanged(); }
    public async Task<bool> Connect(CancellationToken token) {
        token.ThrowIfCancellationRequested();
        await Task.Run(session.Connect, token);
        if (token.IsCancellationRequested) { session.Disconnect(); token.ThrowIfCancellationRequested(); }
        RaiseAllPropertiesChanged(); return true;
    }
    public void Disconnect() { try { session.Disconnect(); } finally { RaiseAllPropertiesChanged(); } }
    public void SetupDialog() {
        if (Connected) CaaSettings.Show(session, ownedByNina: true);
        else {
            using (var setup = NewSession()) CaaSettings.Show(setup, ownedByNina: false);
            session.Dispose(); session = NewSession();
        }
        RaiseAllPropertiesChanged();
    }
    private async Task<bool> Move(string command, float position, CancellationToken token) {
        token.ThrowIfCancellationRequested();
        await Task.Run(() => session.Command(command, position), token);
        try {
            while (true) {
                token.ThrowIfCancellationRequested();
                var status = await Task.Run(session.Status, token); session.CheckMotion(status);
                RaiseAllPropertiesChanged();
                if (!status.Moving) { session.RememberCoordinates(); return true; }
                await Task.Delay(150, token);
            }
        } catch {
            try { session.Halt(); } catch (Exception error) { Logger.Error("ZWOgain CAA halt failed: " + error.Message); }
            throw;
        }
    }
    public Task<bool> Move(float position, CancellationToken ct) => Move("move-relative", position, ct);
    public Task<bool> MoveAbsolute(float position, CancellationToken ct) => Move("move-to", position, ct);
    public Task<bool> MoveAbsoluteMechanical(float position, CancellationToken ct) => Move("move-mechanical", position, ct);
    public void Halt() { session.Halt(); RaiseAllPropertiesChanged(); }
    public IList<string> SupportedActions => CaaSession.Actions.ToList();
    public string Action(string actionName, string actionParameters) => session.Action(actionName, actionParameters);
    public string SendCommandString(string command, bool raw = true) => throw new NotSupportedException("Use supported CAA actions");
    public bool SendCommandBool(string command, bool raw = true) => throw new NotSupportedException("Use supported CAA actions");
    public void SendCommandBlind(string command, bool raw = true) => throw new NotSupportedException("Use supported CAA actions");
    public void Dispose() => session.Dispose();
}
