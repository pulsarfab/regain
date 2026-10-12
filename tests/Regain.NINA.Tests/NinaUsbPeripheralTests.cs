using System.ComponentModel;
using System.Text.Json;
using System.Windows.Controls;
using Moq;
using NINA.Equipment.Equipment.MyFocuser;
using NINA.Equipment.Equipment.MyRotator;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.Mediator;
using NINA.Equipment.Interfaces.ViewModel;
using NINA.Profile.Interfaces;
using NINA.WPF.Base.Mediator;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    public interface IPeripheralChoiceVm { IDeviceChooserVM DeviceChooserVM { get; } }
    private sealed class PeripheralChooser : IDeviceChooserVM, INotifyPropertyChanged
    {
        private IDevice selected;
        internal PeripheralChooser(IDevice selected) { this.selected = selected; Devices = new List<IDevice> { selected }; }
        public IDevice SelectedDevice { get => selected; set { selected = value; PropertyChanged?.Invoke(this, new(nameof(SelectedDevice))); } }
        public bool SetupDialogOpen => false;
        public IList<IDevice> Devices { get; }
        public Task GetEquipment() => Task.CompletedTask;
        public event PropertyChangedEventHandler? PropertyChanged;
    }
    private sealed class PeripheralVm(PeripheralChooser chooser) { public IDeviceChooserVM DeviceChooserVM { get; } = chooser; }
    private sealed class PeripheralFixture : IDisposable
    {
        internal Mock<IFocuserMediator> Mediator { get; } = new();
        internal Mock<IProfileService> Profiles { get; } = new();
        internal Mock<IProfile> Profile { get; } = new();
        internal IDevice Device { get; } = PeripheralDevice("focuser-A");
        internal PeripheralChooser Chooser { get; }
        internal PeripheralVm Vm { get; }
        internal FocuserInfo Info { get; set; } = new() { Connected = true, Position = 12345 };
        internal IDevice? ConnectedDevice { get; set; }
        internal NinaUsbPeripheral<IFocuserVM, IFocuserConsumer, FocuserInfo> Adapter { get; }
        internal PeripheralFixture()
        {
            Profile.SetupGet(profile => profile.Id).Returns(Guid.NewGuid());
            Profiles.SetupGet(service => service.ActiveProfile).Returns(Profile.Object);
            ConnectedDevice = Device; Chooser = new(Device); Vm = new(Chooser);
            Mediator.Setup(mediator => mediator.GetInfo()).Returns(() => Info);
            Mediator.Setup(mediator => mediator.GetDevice()).Returns(() => ConnectedDevice!);
            Adapter = new("focuser", Mediator.Object, Profiles.Object, info => "position=" + info.Position);
        }
        internal void Lost(object? sender = null)
        {
            Info = new FocuserInfo { Connected = false }; ConnectedDevice = null;
            Mediator.Raise(mediator => mediator.Disconnected += null!, sender ?? Vm, EventArgs.Empty);
        }
        public void Dispose() => Adapter.Dispose();
    }
    private static IDevice PeripheralDevice(string id)
    {
        var device = new Mock<IDevice>(); device.SetupGet(value => value.Id).Returns(id); return device.Object;
    }

    [Fact]
    public async Task NinaUsbAdapterLearnsAlreadyConnectedChooserFromRealNinaMediator()
    {
        var device = PeripheralDevice("focuser-A");
        var chooser = new PeripheralChooser(device);
        var profiles = new Mock<IProfileService>(); var profile = new Mock<IProfile>();
        profile.SetupGet(value => value.Id).Returns(Guid.NewGuid()); profiles.SetupGet(value => value.ActiveProfile).Returns(profile.Object);
        var handler = new Mock<IFocuserVM>();
        handler.As<IPeripheralChoiceVm>().SetupGet(value => value.DeviceChooserVM).Returns(chooser);
        FocuserInfo info = new() { Connected = true, Position = 12345 };
        IDevice? current = device;
        handler.Setup(value => value.GetDeviceInfo()).Returns(() => info);
        handler.Setup(value => value.GetDevice()).Returns(() => current!);
        var mediator = new FocuserMediator(); mediator.RegisterHandler(handler.Object);
        using var adapter = new NinaUsbPeripheral<IFocuserVM, IFocuserConsumer, FocuserInfo>("focuser", mediator, profiles.Object, value => "position=" + value.Position);
        Assert.Equal(new PeripheralState(true, "focuser-A", "position=12345"), adapter.Snapshot());
        using var recovery = new UsbPeripheralRecovery([adapter], new() { DisconnectWindowSeconds = 1 }, _ => { });
        recovery.ResetStarting(100);

        // DeviceUpdateTimer reports a polling fault through this same cached
        // info object, without raising NINA's Disconnected event.
        info.Connected = false; info.Position = 0; current = null;
        Assert.Equal(new PeripheralState(false, "focuser-A"), adapter.Snapshot());
        handler.Setup(value => value.Disconnect()).Returns(Task.CompletedTask);
        handler.Setup(value => value.Rescan()).ReturnsAsync((IList<string>)new[] { "focuser-A" });
        handler.Setup(value => value.Connect()).ReturnsAsync(() => { info = new() { Connected = true, Position = 12345 }; current = device; return true; });
        recovery.CameraReturned(100);
        await recovery.WaitAsync(100, CancellationToken.None).WaitAsync(TimeSpan.FromSeconds(5));
        Assert.Equal("position=12345", adapter.Snapshot().ImagingState);
        handler.Verify(value => value.Connect(), Times.Once);
    }

    [Fact]
    public void NinaUsbAdapterDoesNotLearnInconsistentAlreadyConnectedSelection()
    {
        var device = PeripheralDevice("focuser-A");
        var chooser = new PeripheralChooser(PeripheralDevice("focuser-B"));
        var profiles = new Mock<IProfileService>(); var profile = new Mock<IProfile>();
        profile.SetupGet(value => value.Id).Returns(Guid.NewGuid()); profiles.SetupGet(value => value.ActiveProfile).Returns(profile.Object);
        var handler = new Mock<IFocuserVM>();
        handler.As<IPeripheralChoiceVm>().SetupGet(value => value.DeviceChooserVM).Returns(chooser);
        var info = new FocuserInfo { Connected = true, Position = 12345 };
        handler.Setup(value => value.GetDeviceInfo()).Returns(info); handler.Setup(value => value.GetDevice()).Returns(device);
        var mediator = new FocuserMediator(); mediator.RegisterHandler(handler.Object);
        using var adapter = new NinaUsbPeripheral<IFocuserVM, IFocuserConsumer, FocuserInfo>("focuser", mediator, profiles.Object);
        Assert.True(adapter.Snapshot().Connected);
        info.Connected = false;
        Assert.Equal(new PeripheralState(false, ""), adapter.Snapshot());
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task NinaUsbRotatorReconnectWaitsForFreshCompleteCachedPoll(bool cancel)
    {
        var device = PeripheralDevice("rotator-A"); var chooser = new PeripheralChooser(device);
        var profiles = new Mock<IProfileService>(); var profile = new Mock<IProfile>();
        profile.SetupGet(value => value.Id).Returns(Guid.NewGuid()); profiles.SetupGet(value => value.ActiveProfile).Returns(profile.Object);
        var handler = new Mock<IRotatorVM>();
        handler.As<IPeripheralChoiceVm>().SetupGet(value => value.DeviceChooserVM).Returns(chooser);
        RotatorInfo info = new() { Connected = true, Position = 41.2f, MechanicalPosition = 41.2f };
        IDevice? current = device;
        handler.Setup(value => value.GetDeviceInfo()).Returns(() => info);
        handler.Setup(value => value.GetDevice()).Returns(() => current!);
        var mediator = new RotatorMediator(); mediator.RegisterHandler(handler.Object);
        using var adapter = new NinaUsbPeripheral<IRotatorVM, IRotatorConsumer, RotatorInfo>("rotator", mediator, profiles.Object, NinaUsbPeripherals.RotatorState);
        Assert.Equal("rotator-A", adapter.Snapshot().DeviceId);
        info.Connected = false; current = null;
        handler.Setup(value => value.Disconnect()).Returns(() => { info.Connected = false; info.MechanicalPosition = 0; return Task.CompletedTask; });
        handler.Setup(value => value.Rescan()).ReturnsAsync((IList<string>)new[] { "rotator-A" });
        var connected = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        handler.Setup(value => value.Connect()).ReturnsAsync(() => {
            // Real RotatorVM.Connect resets old info, then constructs new info
            // with MechanicalPosition omitted until the next regular poll.
            info.MechanicalPosition = 0;
            info = new() { Connected = true, Position = 41.2f }; current = device;
            handler.Raise(value => value.Connected += null!, handler.Object, EventArgs.Empty);
            connected.TrySetResult(); return true;
        });
        using var cancellation = new CancellationTokenSource(TimeSpan.FromSeconds(5));
        var reconnect = adapter.ReconnectAsync(cancellation.Token);
        await connected.Task.WaitAsync(TimeSpan.FromSeconds(5));
        Assert.False(reconnect.IsCompleted);
        if (cancel) {
            cancellation.Cancel(); await Assert.ThrowsAnyAsync<OperationCanceledException>(() => reconnect);
        } else {
            info.MechanicalPosition = 41.2f;
            Assert.True(await reconnect);
            Assert.Equal("position=41.20;mechanical=41.20;reverse=False;moving=False", adapter.Snapshot().ImagingState);
        }
        handler.Verify(value => value.Move(It.IsAny<float>(), It.IsAny<CancellationToken>()), Times.Never);
        handler.Verify(value => value.MoveMechanical(It.IsAny<float>(), It.IsAny<CancellationToken>()), Times.Never);
    }

    [Fact]
    public void NinaUsbAdapterKeepsChooserIdentityAfterNinaClearsItsConnectedDevice()
    {
        using var fixture = new PeripheralFixture();
        Assert.Equal(new PeripheralState(true, "focuser-A", "position=12345"), fixture.Adapter.Snapshot());
        int changes = 0; fixture.Adapter.StateChanged += () => changes++;
        fixture.Lost();
        Assert.Equal(new PeripheralState(false, "focuser-A"), fixture.Adapter.Snapshot());
        fixture.Chooser.SelectedDevice = PeripheralDevice("focuser-B");
        Assert.Equal(new PeripheralState(false, "focuser-B"), fixture.Adapter.Snapshot());
        Assert.Equal(2, changes);
    }

    [Fact]
    public void NinaUsbAdapterTracksCachedOpticalStateChangesWithoutCallingTheDriver()
    {
        using var fixture = new PeripheralFixture();
        _ = fixture.Adapter.Snapshot(); // Binds NINA's cached DeviceInfo notifications.
        int changes = 0; fixture.Adapter.StateChanged += () => changes++;
        fixture.Info.Position = 23456;
        Assert.True(changes > 0);
        Assert.Equal("position=23456", fixture.Adapter.Snapshot().ImagingState);
        fixture.Mediator.Verify(mediator => mediator.MoveFocuser(It.IsAny<int>(), It.IsAny<CancellationToken>()), Times.Never);
        fixture.Mediator.Verify(mediator => mediator.Connect(), Times.Never);
    }

    [Fact]
    public async Task NinaUsbAdapterFailsClosedWithoutChooserOrAfterProfileChange()
    {
        using var unknown = new PeripheralFixture(); unknown.Lost(new object());
        Assert.Equal(new PeripheralState(false, ""), unknown.Adapter.Snapshot());
        Assert.False(await unknown.Adapter.ReconnectAsync(CancellationToken.None));
        unknown.Mediator.Verify(mediator => mediator.Connect(), Times.Never);
        using var changed = new PeripheralFixture(); changed.Lost();
        changed.Profiles.Raise(service => service.BeforeProfileChanging += null!, changed.Profiles.Object, EventArgs.Empty);
        Assert.Equal(new PeripheralState(false, ""), changed.Adapter.Snapshot());
        Assert.False(await changed.Adapter.ReconnectAsync(CancellationToken.None));
        changed.Mediator.Verify(mediator => mediator.Connect(), Times.Never);
    }

    [Fact]
    public async Task NinaUsbAdapterReconnectsThroughNinaInOrderAndChecksActualIdentity()
    {
        using var fixture = new PeripheralFixture(); fixture.Lost();
        var calls = new List<string>();
        fixture.Mediator.Setup(mediator => mediator.Disconnect()).Returns(() => { calls.Add("disconnect"); fixture.Lost(); return Task.CompletedTask; });
        fixture.Mediator.Setup(mediator => mediator.Rescan()).ReturnsAsync(() => { calls.Add("rescan"); return (IList<string>)new[] { "focuser-A" }; });
        fixture.Mediator.Setup(mediator => mediator.Connect()).ReturnsAsync(() => {
            calls.Add("connect"); fixture.Info = new FocuserInfo { Connected = true, Position = 12345 }; fixture.ConnectedDevice = fixture.Device;
            return true;
        });
        Assert.True(await fixture.Adapter.ReconnectAsync(CancellationToken.None));
        Assert.Equal(new[] { "disconnect", "rescan", "connect" }, calls);
        Assert.Equal("position=12345", fixture.Adapter.Snapshot().ImagingState);
        fixture.Mediator.Verify(mediator => mediator.MoveFocuser(It.IsAny<int>(), It.IsAny<CancellationToken>()), Times.Never);
    }

    [Fact]
    public async Task NinaUsbAdapterNeverConnectsAnotherSelectionAfterRescan()
    {
        using var fixture = new PeripheralFixture(); fixture.Lost();
        fixture.Mediator.Setup(mediator => mediator.Disconnect()).Returns(Task.CompletedTask);
        fixture.Mediator.Setup(mediator => mediator.Rescan()).ReturnsAsync(() => {
            fixture.Chooser.SelectedDevice = PeripheralDevice("focuser-B"); return (IList<string>)new[] { "focuser-A", "focuser-B" };
        });
        Assert.False(await fixture.Adapter.ReconnectAsync(CancellationToken.None));
        fixture.Mediator.Verify(mediator => mediator.Connect(), Times.Never);
    }

    [Fact]
    public async Task NinaUsbAdapterRetainsTimedOutNinaTaskAndForbidsAnOverlappingReconnect()
    {
        using var fixture = new PeripheralFixture(); fixture.Lost();
        var started = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        var unresolved = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        fixture.Mediator.Setup(mediator => mediator.Disconnect()).Returns(() => { started.TrySetResult(true); return unresolved.Task; });
        using var cancellation = new CancellationTokenSource();
        var attempt = fixture.Adapter.ReconnectAsync(cancellation.Token);
        await started.Task.WaitAsync(TimeSpan.FromSeconds(5)); cancellation.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => attempt);
        await Assert.ThrowsAsync<InvalidOperationException>(() => fixture.Adapter.ReconnectAsync(CancellationToken.None));
        fixture.Mediator.Verify(mediator => mediator.Disconnect(), Times.Once);
        unresolved.TrySetResult(true);
        fixture.Mediator.Verify(mediator => mediator.Rescan(), Times.Never);
    }

    [Fact]
    public async Task NinaUsbAdapterRecreatedAfterCameraDisconnectStillCannotOverlapAnOldNinaTask()
    {
        using var fixture = new PeripheralFixture(); fixture.Lost();
        var started = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        var unresolved = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        fixture.Mediator.Setup(mediator => mediator.Disconnect()).Returns(() => { started.TrySetResult(true); return unresolved.Task; });
        using var cancellation = new CancellationTokenSource();
        var attempt = fixture.Adapter.ReconnectAsync(cancellation.Token);
        try {
            await started.Task.WaitAsync(TimeSpan.FromSeconds(5)); cancellation.Cancel();
            await Assert.ThrowsAnyAsync<OperationCanceledException>(() => attempt);
            fixture.Adapter.Dispose(); // Camera disconnect disposes its old adapters.
            using var replacement = new NinaUsbPeripheral<IFocuserVM, IFocuserConsumer, FocuserInfo>(
                "focuser", fixture.Mediator.Object, fixture.Profiles.Object, info => "position=" + info.Position);
            fixture.Lost(); // The new adapter obtains the same retained chooser.
            Assert.Equal("focuser-A", replacement.Snapshot().DeviceId);
            await Assert.ThrowsAsync<InvalidOperationException>(() => replacement.ReconnectAsync(CancellationToken.None));
            fixture.Mediator.Verify(mediator => mediator.Disconnect(), Times.Once);
            fixture.Mediator.Verify(mediator => mediator.Rescan(), Times.Never);
            fixture.Mediator.Verify(mediator => mediator.Connect(), Times.Never);
        } finally { unresolved.TrySetResult(true); }
    }

    [Fact]
    public void NinaUsbAdapterRotatorStateIgnoresTinyReadoutNoiseAndUnknownAngles()
    {
        Assert.Equal(NinaUsbPeripherals.RotatorState(new RotatorInfo { Position = 12.341f, MechanicalPosition = 13.341f }),
            NinaUsbPeripherals.RotatorState(new RotatorInfo { Position = 12.342f, MechanicalPosition = 13.342f }));
        Assert.NotEqual(NinaUsbPeripherals.RotatorState(new RotatorInfo { Position = 12.34f, MechanicalPosition = 13.34f }),
            NinaUsbPeripherals.RotatorState(new RotatorInfo { Position = 22.34f, MechanicalPosition = 23.34f }));
        Assert.Null(NinaUsbPeripherals.RotatorState(new RotatorInfo { Position = float.NaN }));
    }

    [Fact]
    public async Task NinaUsbPeripheralSettingsKeepSeparateDefaultsAndBoundTheDisconnectWindow()
    {
        string directory = Path.Combine(Path.GetTempPath(), "regain-peripherals-" + Guid.NewGuid().ToString("N"));
        string path = Path.Combine(directory, "camera-peripherals.json");
        try {
            var defaults = Settings.LoadPeripheralRecovery(path);
            Assert.True(defaults.Enabled); Assert.Equal(15, defaults.DisconnectWindowSeconds); Assert.False(File.Exists(path));
            var changed = new UsbPeripheralRecoveryOptions { Enabled = false, DisconnectWindowSeconds = 60 };
            Settings.SavePeripheralRecovery(changed, path); Assert.Equal(changed, Settings.LoadPeripheralRecovery(path));
            using var json = JsonDocument.Parse(File.ReadAllText(path)); Assert.Equal(2, json.RootElement.EnumerateObject().Count());
            foreach (int invalid in new[] { 0, 61 }) Assert.ThrowsAny<ArgumentException>(() => Settings.SavePeripheralRecovery(new() { DisconnectWindowSeconds = invalid }, path));
            await Wpf(() => {
                var body = new StackPanel(); var read = Settings.PeripheralRecoveryControls(body, defaults);
                var enabled = body.Children.OfType<CheckBox>().Single(); var window = body.Children.OfType<TextBox>().Single();
                Assert.True(enabled.IsChecked); Assert.Null(enabled.Content); Assert.Equal("15", window.Text);
                foreach (string invalid in new[] { "0", "61", "1.5" }) { window.Text = invalid; Assert.ThrowsAny<ArgumentException>(() => read()); }
                enabled.IsChecked = false; window.Text = "1"; Assert.Equal(new UsbPeripheralRecoveryOptions { Enabled = false, DisconnectWindowSeconds = 1 }, read());
                return Task.CompletedTask;
            });
        } finally { if (File.Exists(path)) File.Delete(path); if (Directory.Exists(directory)) Directory.Delete(directory); }
    }
}
