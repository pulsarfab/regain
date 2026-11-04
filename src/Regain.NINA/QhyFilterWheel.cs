using System.ComponentModel.Composition;
using NINA.Core.Model.Equipment;
using NINA.Core.Utility;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.ViewModel;
using NINA.Profile.Interfaces;
using Regain.Core;

namespace Regain.NINA;

[Export(typeof(IEquipmentProvider))]
public sealed class QhyFilterWheelProvider : IEquipmentProvider<IFilterWheel>
{
    private readonly IProfileService profiles;
    [ImportingConstructor] public QhyFilterWheelProvider(IProfileService profiles) => this.profiles = profiles;
    public string Name => "PulsarFab regain";
    public IList<IFilterWheel> GetEquipment() => [new QhyFilterWheel(profiles)];
}

public sealed class QhyFilterWheel(IProfileService profiles) : BaseINPC, IFilterWheel, IDisposable
{
    private QhyCameraConnections.Lease? lease;
    private readonly SemaphoreSlim connection = new(1);
    private CameraSession Session => lease?.Session ?? throw new InvalidOperationException("QHY filter wheel is disconnected");
    public string Id => "Regain.QHY.CFW";
    public string Name => "PulsarFab regain QHY Filter Wheel (camera 4-pin)";
    public string DisplayName => Name;
    public string Category => "PulsarFab regain";
    public string Description => "Shares the parent QHY camera and backend selected in Regain camera setup";
    public string DriverInfo => Connected ? $"Shared {Session.Backend} camera connection; optical checks enabled" : Description;
    public string DriverVersion => typeof(QhyFilterWheel).Assembly.GetName().Version!.ToString();
    public bool Connected => lease is not null;
    public bool HasSetupDialog => true;
    public AsyncObservableCollection<FilterInfo> Filters => profiles.ActiveProfile.FilterWheelSettings.FilterWheelFilters;
    public string[] Names => Filters.Select(f => f.Name).ToArray();
    public int[] FocusOffsets => Filters.Select(f => f.FocusOffset).ToArray();
    public short Position {
        get { var state = Session.FilterWheelStatusAsync().GetAwaiter().GetResult(); return state.Moving ? (short)-1 : checked((short)state.Position); }
        set { Session.MoveFilterWheelAsync(value).GetAwaiter().GetResult(); RaisePropertyChanged(); }
    }
    public async Task<bool> Connect(CancellationToken token)
    {
        await connection.WaitAsync(token).ConfigureAwait(false);
        try {
            if (Connected) return true;
            var selection = Settings.Cameras.Load() ?? throw new InvalidOperationException("Choose the parent QHY camera in Regain camera setup first");
            var candidate = await QhyCameraConnections.AcquireAsync(selection, camera: false, token).ConfigureAwait(false);
            try {
                var state = await candidate.Session.FilterWheelStatusAsync(token).ConfigureAwait(false);
                if (state.Moving || !state.Verified) throw new IOException("Wait for the wheel to settle before connecting");
                for (int i = Filters.Count; i < state.Slots; i++) Filters.Add(new FilterInfo($"Filter {i+1}", 0, (short)i));
                while (Filters.Count > state.Slots) Filters.RemoveAt(Filters.Count-1);
                lease = candidate;
            } catch { candidate.Dispose(); throw; }
            RaiseAllPropertiesChanged(); return true;
        } finally { connection.Release(); }
    }
    public void Disconnect() {
        connection.Wait();
        try { var old = lease; lease = null; old?.Dispose(); }
        finally { connection.Release(); RaiseAllPropertiesChanged(); }
    }
    public void SetupDialog() => Settings.Show(QhyCameraConnections.UpdateOptionsAsync, () => QhyCameraConnections.InUse);
    public IList<string> SupportedActions => ["Regain.Diagnostics"];
    public string Action(string actionName, string actionParameters) => actionName.Equals("Regain.Diagnostics", StringComparison.OrdinalIgnoreCase)
        ? System.Text.Json.JsonSerializer.Serialize(Session.FilterWheelStatusAsync().GetAwaiter().GetResult()) : throw new NotSupportedException(actionName);
    public string SendCommandString(string command, bool raw = true) => throw new NotSupportedException();
    public bool SendCommandBool(string command, bool raw = true) => throw new NotSupportedException();
    public void SendCommandBlind(string command, bool raw = true) => throw new NotSupportedException();
    public void Dispose() => Disconnect();
}
