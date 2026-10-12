using System.ComponentModel;
using System.Globalization;
using System.Reflection;
using System.Runtime.CompilerServices;
using NINA.Equipment.Equipment;
using NINA.Equipment.Equipment.MyFilterWheel;
using NINA.Equipment.Equipment.MyFlatDevice;
using NINA.Equipment.Equipment.MyFocuser;
using NINA.Equipment.Equipment.MyRotator;
using NINA.Equipment.Interfaces.Mediator;
using NINA.Equipment.Interfaces.ViewModel;
using NINA.Profile.Interfaces;
using NINA.WPF.Base.Mediator;

namespace Regain.NINA;

internal static class NinaUsbPeripherals
{
    internal static IReadOnlyList<IUsbPeripheral> Create(IProfileService profiles,
        IFocuserMediator focuser, IFilterWheelMediator wheel, IRotatorMediator rotator, IGuiderMediator guider,
        ISwitchMediator switches, IFlatDeviceMediator flat, IWeatherDataMediator weather,
        ISafetyMonitorMediator safety, ITelescopeMediator telescope, IDomeMediator dome) => [
            new NinaUsbPeripheral<IFocuserVM, IFocuserConsumer, FocuserInfo>("focuser", focuser, profiles,
                info => $"position={info.Position};moving={info.IsMoving};settling={info.IsSettling}"),
            new NinaUsbPeripheral<IFilterWheelVM, IFilterWheelConsumer, FilterWheelInfo>("filter wheel", wheel, profiles,
                info => info.SelectedFilter is null ? null : $"filter={info.SelectedFilter.Position};moving={info.IsMoving}"),
            new NinaUsbPeripheral<IRotatorVM, IRotatorConsumer, RotatorInfo>("rotator", rotator, profiles, RotatorState),
            new NinaUsbPeripheral<IGuiderVM, IGuiderConsumer, global::NINA.Equipment.Equipment.MyGuider.GuiderInfo>("guider", guider, profiles),
            new NinaUsbPeripheral<ISwitchVM, ISwitchConsumer, global::NINA.Equipment.Equipment.MySwitch.SwitchInfo>("switch", switches, profiles),
            new NinaUsbPeripheral<IFlatDeviceVM, IFlatDeviceConsumer, FlatDeviceInfo>("flat panel", flat, profiles, FlatState),
            new NinaUsbPeripheral<IWeatherDataVM, IWeatherDataConsumer, global::NINA.Equipment.Equipment.MyWeatherData.WeatherDataInfo>("weather", weather, profiles),
            new NinaUsbPeripheral<ISafetyMonitorVM, ISafetyMonitorConsumer, global::NINA.Equipment.Equipment.MySafetyMonitor.SafetyMonitorInfo>("safety monitor", safety, profiles),
            new NinaUsbPeripheral<ITelescopeVM, ITelescopeConsumer, global::NINA.Equipment.Equipment.MyTelescope.TelescopeInfo>("telescope", telescope, profiles),
            new NinaUsbPeripheral<IDomeVM, IDomeConsumer, global::NINA.Equipment.Equipment.MyDome.DomeInfo>("dome", dome, profiles)
        ];

    internal static string? RotatorState(RotatorInfo info) => float.IsNaN(info.Position) || float.IsInfinity(info.Position) ||
        float.IsNaN(info.MechanicalPosition) || float.IsInfinity(info.MechanicalPosition) ? null :
        "position=" + Math.Round(info.Position, 2).ToString("F2", CultureInfo.InvariantCulture) +
        ";mechanical=" + Math.Round(info.MechanicalPosition, 2).ToString("F2", CultureInfo.InvariantCulture) +
        $";reverse={info.Reverse};moving={info.IsMoving}";

    internal static string? FlatState(FlatDeviceInfo info) => !info.SupportsOnOff && !info.SupportsOpenClose ? null :
        (info.SupportsOpenClose ? "cover=" + info.CoverState + ";" : "") +
        (info.SupportsOnOff ? $"light={info.LightOn};brightness={info.Brightness}" : "");
}

/// NINA owns connection and device choice. This bridge never invokes a device
/// driver directly, changes a selected device, moves equipment or starts guiding.
internal sealed class NinaUsbPeripheral<THandler, TConsumer, TInfo> : IUsbPeripheral
    where THandler : IDeviceVM<TInfo>
    where TConsumer : IDeviceConsumer<TInfo>
    where TInfo : DeviceInfo
{
    // NINA connection tasks have no cancellation contract. Retain ownership by
    // mediator identity across camera disconnects and newly created adapters.
    private sealed class OperationLedger
    {
        internal readonly object Gate = new();
        internal Task? Pending;
        internal bool ActiveSequence;
    }
    private static readonly ConditionalWeakTable<object, OperationLedger> Operations = new();
    private readonly IDeviceMediator<THandler, TConsumer, TInfo> mediator;
    private readonly IProfileService profiles;
    private readonly Guid profileId;
    private readonly Func<TInfo, string?>? imagingState;
    private readonly object gate = new();
    private readonly OperationLedger operations;
    private IDeviceChooserVM? chooser;
    private INotifyPropertyChanged? chooserChanges;
    private INotifyPropertyChanged? infoChanges;
    private INotifyPropertyChanged? beforeRotatorConnect;
    private TaskCompletionSource? rotatorRefresh;
    private bool disposed, profileChanged;
    public string Kind { get; }
    public bool RequiresImagingState => imagingState is not null;
    public event Action? StateChanged;

    internal NinaUsbPeripheral(string kind, IDeviceMediator<THandler, TConsumer, TInfo> mediator,
        IProfileService profiles, Func<TInfo, string?>? imagingState = null)
    {
        Kind = kind; this.mediator = mediator; this.profiles = profiles; this.imagingState = imagingState;
        operations = Operations.GetValue(mediator, _ => new());
        profileId = profiles.ActiveProfile.Id;
        mediator.Connected += ConnectionChanged; mediator.Disconnected += ConnectionChanged;
        profiles.BeforeProfileChanging += BeforeProfileChanging;
    }

    public PeripheralState Snapshot()
    {
        IDeviceChooserVM? selection;
        lock (gate) {
            if (disposed || profileChanged) return new(false, "");
            selection = chooser;
        }
        // NINA raises chooser PropertyChanged while holding its own selection
        // lock. Never call NINA getters under our gate or invert that lock order.
        if (profiles.ActiveProfile.Id != profileId) return new(false, "");
        var info = mediator.GetInfo();
        BindInfo(info);
        if (info?.Connected == true) {
            var device = mediator.GetDevice();
            BindExistingChooser(info, device);
            return new(true, device?.Id ?? "", imagingState?.Invoke(info));
        }
        // GetDevice() and DeviceInfo are cleared before NINA's Disconnected
        // event. Read the chooser retained by that event's VM instead.
        return new(false, ValidId(selection?.SelectedDevice?.Id));
    }

    private static string ValidId(string? id) => string.IsNullOrEmpty(id) || id == "No_Device" ? "" : id;
    private bool SameSelection(string expected)
    {
        IDeviceChooserVM? selection;
        lock (gate) { if (disposed || profileChanged) return false; selection = chooser; }
        return profiles.ActiveProfile.Id == profileId && !string.IsNullOrEmpty(expected) &&
            ValidId(selection?.SelectedDevice?.Id) == expected;
    }

    private Task ConnectionChanged(object sender, EventArgs args)
    {
        // IDeviceVM<T> does not publish a chooser. Actual NINA equipment VMs
        // expose this public property, while unknown implementations fail closed.
        IDeviceChooserVM? found = null;
        try { found = sender?.GetType().GetProperty("DeviceChooserVM", BindingFlags.Instance | BindingFlags.Public)?.GetValue(sender) as IDeviceChooserVM; }
        catch { }
        BindChooser(found);
        BindInfo(mediator.GetInfo());
        Notify(); return Task.CompletedTask; // Never hold up NINA's disconnect lifecycle.
    }

    private void BindExistingChooser(TInfo info, global::NINA.Equipment.Interfaces.IDevice? device)
    {
        lock (gate) if (disposed || chooser is not null) return;
        // A device may already be connected when the camera creates its adapters.
        // NINA's mediator API has no handler getter, and polling failures update
        // DeviceInfo without raising Disconnected. Inspect only NINA's known
        // handler cache; never guess a VM or select a device by its display name.
        if (mediator is not DeviceMediator<THandler, TConsumer, TInfo> || device is null) return;
        try {
            if (typeof(DeviceMediator<THandler, TConsumer, TInfo>)
                .GetField("handler", BindingFlags.Instance | BindingFlags.NonPublic)?.GetValue(mediator) is not THandler handler ||
                !ReferenceEquals(handler.GetDeviceInfo(), info) || !ReferenceEquals(handler.GetDevice(), device)) return;
            var found = handler.GetType().GetProperty("DeviceChooserVM", BindingFlags.Instance | BindingFlags.Public)?.GetValue(handler) as IDeviceChooserVM;
            if (found is null || string.IsNullOrEmpty(ValidId(device.Id)) || ValidId(found.SelectedDevice?.Id) != device.Id) return;
            BindChooser(found);
        } catch { /* An unknown mediator or inconsistent selection fails closed. */ }
    }

    private void BindChooser(IDeviceChooserVM? found)
    {
        lock (gate) {
            if (disposed) return;
            if (found is not null && !ReferenceEquals(found, chooser)) {
                if (chooserChanges is not null) chooserChanges.PropertyChanged -= SelectionChanged;
                chooser = found; chooserChanges = found as INotifyPropertyChanged;
                if (chooserChanges is not null) chooserChanges.PropertyChanged += SelectionChanged;
            }
        }
    }

    private void BindInfo(TInfo? info)
    {
        var notifications = info as INotifyPropertyChanged;
        lock (gate) {
            if (disposed || ReferenceEquals(infoChanges, notifications)) return;
            if (infoChanges is not null) infoChanges.PropertyChanged -= InfoChanged;
            infoChanges = notifications;
            if (infoChanges is not null) infoChanges.PropertyChanged += InfoChanged;
        }
    }
    private void InfoChanged(object? sender, PropertyChangedEventArgs args)
    {
        lock (gate) {
            // RotatorVM's initial Connected info omits MechanicalPosition.
            // Its regular cached update writes that field last. An old info
            // object reset during Connect's own Disconnect is not a fresh poll.
            if (rotatorRefresh is { } refresh && ReferenceEquals(sender, infoChanges) &&
                !ReferenceEquals(sender, beforeRotatorConnect) && sender is RotatorInfo { Connected: true } &&
                args.PropertyName == nameof(RotatorInfo.MechanicalPosition)) refresh.TrySetResult();
        }
        Notify();
    }

    private void SelectionChanged(object? sender, PropertyChangedEventArgs args)
    {
        if (string.IsNullOrEmpty(args.PropertyName) || args.PropertyName == nameof(IDeviceChooserVM.SelectedDevice)) Notify();
    }
    private void BeforeProfileChanging(object? sender, EventArgs args)
    {
        lock (gate) profileChanged = true;
        Notify();
    }
    private void Notify()
    {
        var listeners = StateChanged;
        if (listeners is null) return;
        foreach (Action listener in listeners.GetInvocationList()) {
            try { listener(); } catch (Exception error) { CameraLog.Session("USB peripherals", Kind + " observer failed: " + error.Message); }
        }
    }

    public async Task<bool> ReconnectAsync(CancellationToken token)
    {
        token.ThrowIfCancellationRequested();
        IDeviceChooserVM? selection;
        lock (gate) selection = chooser;
        string expected = ValidId(selection?.SelectedDevice?.Id);
        if (!SameSelection(expected)) return false;
        lock (operations.Gate) {
            if (operations.ActiveSequence || operations.Pending is { IsCompleted: false }) throw new InvalidOperationException("The previous NINA " + Kind + " operation is still unresolved; automatic reconnect cannot overlap it.");
            if (operations.Pending?.IsFaulted == true) _ = operations.Pending.Exception; // Observe a late failure; never replay an unresolved call.
            operations.ActiveSequence = true;
        }
        try {
            if (!await Step(mediator.Disconnect, expected, token).ConfigureAwait(false) || !SameSelection(expected)) return false;
            var available = await StepResult(mediator.Rescan, expected, token).ConfigureAwait(false);
            if (available is null || !available.Contains(expected, StringComparer.Ordinal) || !SameSelection(expected)) return false;
            Task? freshRotatorInfo = null;
            if (typeof(TInfo) == typeof(RotatorInfo)) {
                lock (gate) {
                    beforeRotatorConnect = infoChanges;
                    rotatorRefresh = new(TaskCreationOptions.RunContinuationsAsynchronously);
                    freshRotatorInfo = rotatorRefresh.Task;
                }
            }
            bool connected = await StepResult(mediator.Connect, expected, token).ConfigureAwait(false);
            if (!connected || !SameSelection(expected)) return false;
            // Wait only for NINA's cached poll, never issue a driver read or move.
            // The coordinator's per-attempt deadline also bounds this wait.
            if (freshRotatorInfo is not null) await freshRotatorInfo.WaitAsync(token).ConfigureAwait(false);
            if (!SameSelection(expected)) return false;
            var actual = Snapshot();
            return actual.Connected && actual.DeviceId == expected;
        } finally {
            lock (gate) { rotatorRefresh = null; beforeRotatorConnect = null; }
            lock (operations.Gate) operations.ActiveSequence = false;
        }
    }

    private async Task<bool> Step(Func<Task> start, string expected, CancellationToken token)
    {
        Task<bool> work;
        token.ThrowIfCancellationRequested(); if (!SameSelection(expected)) return false;
        lock (operations.Gate) {
            token.ThrowIfCancellationRequested();
            operations.Pending = work = Task.Run(async () => {
                token.ThrowIfCancellationRequested(); if (!SameSelection(expected)) return false;
                await start().ConfigureAwait(false); return true;
            }, token);
        }
        return await work.WaitAsync(token).ConfigureAwait(false);
    }
    private async Task<TResult> StepResult<TResult>(Func<Task<TResult>> start, string expected, CancellationToken token)
    {
        Task<TResult> work;
        token.ThrowIfCancellationRequested(); if (!SameSelection(expected)) return default!;
        lock (operations.Gate) {
            token.ThrowIfCancellationRequested();
            operations.Pending = work = Task.Run(async () => {
                token.ThrowIfCancellationRequested(); if (!SameSelection(expected)) return default!;
                return await start().ConfigureAwait(false);
            }, token);
        }
        return await work.WaitAsync(token).ConfigureAwait(false);
    }

    public void Dispose()
    {
        INotifyPropertyChanged? notifications, information;
        lock (gate) {
            if (disposed) return; disposed = true;
            notifications = chooserChanges; chooserChanges = null;
            information = infoChanges; infoChanges = null;
            rotatorRefresh?.TrySetCanceled(); rotatorRefresh = null; beforeRotatorConnect = null;
            StateChanged = null;
        }
        mediator.Connected -= ConnectionChanged; mediator.Disconnected -= ConnectionChanged;
        profiles.BeforeProfileChanging -= BeforeProfileChanging;
        if (notifications is not null) notifications.PropertyChanged -= SelectionChanged;
        if (information is not null) information.PropertyChanged -= InfoChanged;
    }
}
