using NINA.Core.Enum;
using NINA.Core.Model.Equipment;
using NINA.Core.Utility;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Model;
using NINA.Equipment.Utility;
using NINA.Image.ImageData;
using NINA.Image.Interfaces;
using NINA.Profile.Interfaces;
using ZwoGain.Core;

namespace ZwoGain.NINA;

public sealed class ResilientCamera : BaseINPC, ICamera
{
    private CameraDescriptor descriptor;
    private readonly CameraSelectionStore? selectionStore;
    private readonly IExposureDataFactory images;
    private readonly Func<HostClient> hostFactory;
    private readonly bool useConfiguredBackend;
    private readonly RecoveryOptions? recoveryOptions;
    private readonly IProfileService? profiles;
    private ICameraSettings? timeoutSettings;
    private int originalTimeout, extendedTimeout;
    private readonly object sync = new();
    private CameraSession? session;
    private CancellationTokenSource? lifetime, exposureCancel;
    private Task<Frame>? exposure;
    private Task? telemetry;
    private short bin = 1;
    public ResilientCamera(CameraDescriptor camera, IExposureDataFactory images) : this(camera, images, CameraProvider.NewHost, null) { }
    internal ResilientCamera(IExposureDataFactory images, CameraSelectionStore store, Func<HostClient>? factory = null, RecoveryOptions? options = null, IProfileService? profiles = null)
        : this(store.Load()?.Camera ?? new("Select a camera", 0, 0, false, 0, 0, 16, false, false, [1]), images, factory ?? CameraProvider.NewHost, options, profiles)
    {
        selectionStore = store;
        useConfiguredBackend = factory is null;
    }
    internal ResilientCamera(CameraDescriptor camera, IExposureDataFactory images, Func<HostClient> hostFactory, RecoveryOptions? options, IProfileService? profiles = null)
    {
        descriptor = camera;
        this.images = images;
        this.hostFactory = hostFactory;
        recoveryOptions = options;
        this.profiles = profiles;
        SubSampleWidth = camera.Width;
        SubSampleHeight = camera.Height;
    }
    private CameraSession Session => session ?? throw new InvalidOperationException("Camera is disconnected");
    private bool Has(int c) => session?.Controls.TryGetValue(c, out var cap) == true && cap.Writable;
    private int Val(int c, int fallback = 0) => (int)(session?.Value(c, fallback) ?? fallback);
    private int Min(int c) => (int)(session?.Controls.GetValueOrDefault(c)?.Min ?? 0);
    private int Max(int c) => (int)(session?.Controls.GetValueOrDefault(c)?.Max ?? 0);
    private void Set(int c, long value)
    {
        Session.Set(c, value);
        RaiseAllPropertiesChanged();
    }
    public string Id => "ZwoGain";
    public string Name => Settings.CameraLabel(descriptor.Name);
    public string DisplayName => "ZWOgain Retryable Camera";
    public string Category => "ZWOgain";
    public string Description => "ZWO camera driver with automatic retries";
    public string DriverInfo => $"ZWOgain {DriverVersion} / {session?.SdkVersion} [{session?.Backend}{(session?.UsingSdkFallback == true ? " fallback" : "")}]; {session?.Phase}";
    public string DriverVersion => typeof(ResilientCamera).Assembly.GetName().Version!.ToString();
    public bool Connected
    {
        get; private set;
    }
    public bool HasSetupDialog => true;
    public void SetupDialog()
    {
        Settings.Show();
        if (!Connected && selectionStore?.Load() is { } selected)
            SelectCamera(selected.Camera);
    }
    private void SelectCamera(CameraDescriptor camera)
    {
        descriptor = camera;
        bin = 1;
        EnableSubSample = false;
        SubSampleX = SubSampleY = 0;
        SubSampleWidth = camera.Width;
        SubSampleHeight = camera.Height;
        exposure = null;
        RaiseAllPropertiesChanged();
    }
    public async Task<bool> Connect(CancellationToken token)
    {
        if (Connected)
            return true;
        CameraSelection? selected = null;
        if (selectionStore is not null)
        {
            selected = selectionStore.Load() ?? throw new InvalidOperationException("Choose a camera in the ZWOgain setup dialog first.");
            SelectCamera(selected.Camera);
        }
        bool direct = selected?.UseDirectDriver == true;
        if (direct && descriptor.Name is not ("ZWO ASI676MC" or "ZWO ASI2600MM Duo" or "ZWO ASI220MM Mini"))
            throw new NotSupportedException("Direct capture is unavailable for this camera. Turn off Direct USB driver to use the SDK.");
        var factory = useConfiguredBackend && direct ? CameraProvider.NewDirectHost : hostFactory;
        var candidate = new CameraSession(descriptor, factory, recoveryOptions ?? Settings.Load(), selected?.Serial, direct && selected?.AllowSdkFallback == true ? hostFactory : null);
        candidate.Diagnostic += text => Logger.Info($"ZWOgain {Name}: {text}");
        try
        {
            await candidate.ConnectAsync(token).ConfigureAwait(false);
            SelectCamera(candidate.Camera); // Backend capabilities may be narrower than saved SDK capabilities.
            // Match the native ASI driver's acquisition defaults. Preserve cooling state.
            foreach (var (control, value) in new Dictionary<int, long> { { 2, 50 }, { 3, 50 }, { 4, 50 }, { 6, 40 }, { 7, 0 }, { 9, 0 }, { 13, 0 }, { 14, 0 }, { 18, 0 }, { 20, 0 } })
                if (candidate.Controls.TryGetValue(control, out var cap) && cap.Writable && value >= cap.Min && value <= cap.Max)
                    candidate.Set(control, value);
            await candidate.RefreshAsync(token).ConfigureAwait(false);
            if (selected is not null)
                selectionStore!.RememberSerial(selected, candidate.Serial);
            session = candidate;
            lifetime = new();
            Connected = true;
            telemetry = Poll(candidate, lifetime.Token);
            RaiseAllPropertiesChanged();
            return true;
        }
        catch { candidate.Dispose(); throw; }
    }
    private async Task Poll(CameraSession owner, CancellationToken token)
    {
        while (!token.IsCancellationRequested)
        {
            try
            {
                await Task.Delay(2000, token).ConfigureAwait(false);
                await owner.RefreshAsync(token).ConfigureAwait(false);
                RaiseAllPropertiesChanged();
            }
            catch (OperationCanceledException) { return; }
            catch (Exception e) { Logger.Warning($"ZWOgain telemetry: {e.Message}"); }
        }
    }
    public void Disconnect()
    {
        lock (sync)
        {
            lifetime?.Cancel();
            exposureCancel?.Cancel();
            session?.Dispose();
            session = null;
            Connected = false;
            RestoreNinaTimeout();
        }
        RaiseAllPropertiesChanged();
    }
    public bool HasShutter => descriptor.Shutter;
    public string SensorName => string.Empty;
    public SensorType SensorType => !descriptor.Color ? SensorType.Monochrome : descriptor.Bayer switch { 0 => SensorType.RGGB, 1 => SensorType.BGGR, 2 => SensorType.GRBG, 3 => SensorType.GBRG, _ => SensorType.Color };
    public short BayerOffsetX => (short)(EnableSubSample ? (SubSampleX / bin) % 2 : 0);
    public short BayerOffsetY => (short)(EnableSubSample ? (SubSampleY / bin) % 2 : 0);
    public int CameraXSize => descriptor.Width;
    public int CameraYSize => descriptor.Height;
    public double PixelSizeX => descriptor.PixelSize;
    public double PixelSizeY => descriptor.PixelSize;
    // SDK RAW16 is left-aligned, as in the native NINA ASI driver.
    public int BitDepth => 16;
    public double ExposureMin => Min(1) / 1e6;
    public double ExposureMax => Max(1) / 1e6;
    public short BinX
    {
        get => bin; set => SetBinning(value, value);
    }
    public short BinY
    {
        get => bin; set => SetBinning(value, value);
    }
    public short MaxBinX => (short)descriptor.Bins.Max();
    public short MaxBinY => MaxBinX;
    public AsyncObservableCollection<BinningMode> BinningModes => new(descriptor.Bins.Select(b => new BinningMode((short)b, (short)b)));
    public void SetBinning(short x, short y)
    {
        if (x != y || !descriptor.Bins.Contains(x))
            throw new ArgumentOutOfRangeException(nameof(x));
        bin = x;
        RaisePropertyChanged(nameof(BinX));
        RaisePropertyChanged(nameof(BinY));
    }
    public double Temperature => session?.Controls.ContainsKey(8) != true ? double.NaN : Val(8) / 10.0;
    public double TemperatureSetPoint
    {
        get => CanSetTemperature ? Val(16) : double.NaN; set => Set(16, (long)Math.Clamp(Math.Round(value), Min(16), Max(16)));
    }
    public bool CanSetTemperature => Has(16) && Has(17);
    public bool CoolerOn
    {
        get => Val(17) != 0; set => Set(17, value ? 1 : 0);
    }
    public double CoolerPower => Val(15);
    public bool HasDewHeater => Has(21);
    public bool DewHeaterOn
    {
        get => Val(21) != 0; set => Set(21, value ? 1 : 0);
    }
    public bool CanGetGain => session?.Controls.ContainsKey(0) == true;
    public bool CanSetGain => Has(0);
    public int Gain
    {
        get => Val(0); set => Set(0, value);
    }
    public int GainMin => Min(0);
    public int GainMax => Max(0);
    public IList<int> Gains => new List<int>();
    public double ElectronsPerADU => double.NaN;
    public bool CanSetOffset => Has(5);
    public int Offset
    {
        get => Val(5); set => Set(5, value);
    }
    public int OffsetMin => Min(5);
    public int OffsetMax => Max(5);
    public bool CanSetUSBLimit => Has(6);
    public int USBLimit
    {
        get => Val(6); set => Set(6, value);
    }
    public int USBLimitMin => Min(6);
    public int USBLimitMax => Max(6);
    public int USBLimitStep => 1;
    public bool CanSubSample => true;
    public bool EnableSubSample
    {
        get; set;
    }
    public int SubSampleX
    {
        get; set;
    }
    public int SubSampleY
    {
        get; set;
    }
    public int SubSampleWidth
    {
        get; set;
    }
    public int SubSampleHeight
    {
        get; set;
    }
    public void UpdateSubSampleArea()
    {
    }
    public CameraStates CameraState => !Connected ? CameraStates.NoState : exposure?.IsFaulted == true ? CameraStates.Error : exposure is { IsCompleted: false } ? (session?.Phase == "Downloading" ? CameraStates.Download : CameraStates.Exposing) : CameraStates.Idle;
    public IList<string> ReadoutModes => new List<string> { "RAW16" };
    public short ReadoutMode
    {
        get => 0; set
        {
            if (value != 0)
                throw new ArgumentOutOfRangeException(nameof(value));
        }
    }
    public short ReadoutModeForSnapImages
    {
        get => 0; set => ReadoutMode = value;
    }
    public short ReadoutModeForNormalImages
    {
        get => 0; set => ReadoutMode = value;
    }
    public bool HasBattery => false;
    public int BatteryLevel => -1;
    public bool CanShowLiveView => false;
    public bool LiveViewEnabled => false;
    public void StartLiveView(CaptureSequence sequence) => throw new NotSupportedException();
    public Task<IExposureData> DownloadLiveView(CancellationToken token) => throw new NotSupportedException();
    public void StopLiveView()
    {
    }
    public void StartExposure(CaptureSequence sequence)
    {
        lock (sync)
        {
            if (exposure is { IsCompleted: false })
                throw new InvalidOperationException("Exposure already active");
            SetBinning(sequence.Binning.X, sequence.Binning.Y);
            if (sequence.Gain >= 0 && CanSetGain)
                Gain = sequence.Gain;
            if (sequence.Offset >= 0 && CanSetOffset)
                Offset = sequence.Offset;
            int width = (EnableSubSample ? SubSampleWidth : CameraXSize) / bin, height = (EnableSubSample ? SubSampleHeight : CameraYSize) / bin;
            width -= width % 8;
            height -= height % 2;
            var request = new Exposure(width, height, bin, EnableSubSample ? SubSampleX / bin : 0, EnableSubSample ? SubSampleY / bin : 0, checked((long)Math.Round(sequence.ExposureTime * 1e6)), sequence.IsDarkSequence());
            exposureCancel?.Dispose();
            exposureCancel = CancellationTokenSource.CreateLinkedTokenSource(lifetime?.Token ?? CancellationToken.None);
            // Session stays alive until this task unwinds; no SDK work on NINA's UI thread.
            var owner = Session;
            var token = exposureCancel.Token;
            ExtendNinaTimeout(owner, sequence.ExposureTime);
            exposure = Task.Run(() => owner.CaptureAsync(request, token), token);
        }
    }
    public async Task WaitUntilExposureIsReady(CancellationToken token)
    {
        Task<Frame> pending;
        lock (sync)
            pending = exposure ?? throw new InvalidOperationException("No exposure");
        using var cancel = token.Register(AbortExposure);
        try { await pending.WaitAsync(token).ConfigureAwait(false); }
        catch { lock (sync) RestoreNinaTimeout(); throw; }
    }
    public async Task<IExposureData> DownloadExposure(CancellationToken token)
    {
        Task<Frame> pending;
        lock (sync)
            pending = exposure ?? throw new InvalidOperationException("No exposure");
        using var cancel = token.Register(AbortExposure);
        Frame frame;
        try { frame = await pending.WaitAsync(token).ConfigureAwait(false); }
        finally { lock (sync) RestoreNinaTimeout(); }
        var metadata = new ImageMetaData();
        metadata.FromCamera(this);
        metadata.Image.ExposureStart = frame.StartedUtc;
        metadata.Image.ExposureTime = frame.Exposure.microseconds / 1e6;
        metadata.Camera.BinX = metadata.Camera.BinY = frame.Exposure.bin;
        metadata.Camera.Gain = (int)frame.Controls.GetValueOrDefault(0);
        metadata.Camera.Offset = (int)frame.Controls.GetValueOrDefault(5);
        metadata.Camera.BayerOffsetX = frame.Exposure.x % 2;
        metadata.Camera.BayerOffsetY = frame.Exposure.y % 2;
        bool bayered = descriptor.Color && !(frame.Controls.GetValueOrDefault(18) != 0 && frame.Exposure.bin > 1);
        if (!bayered)
            metadata.Camera.BayerPattern = BayerPatternEnum.None;
        return images.CreateImageArrayExposureData(frame.Pixels, frame.Width, frame.Height, BitDepth, bayered, metadata);
    }
    public void StopExposure() => AbortExposure();
    public void AbortExposure()
    {
        lock (sync)
        {
            exposureCancel?.Cancel();
            RestoreNinaTimeout();
        }
    }
    // NINA 3.2 imposes its own exposure-time + profile timeout around readiness.
    // Temporarily budget for our bounded recovery before NINA starts that clock.
    // Keep real readiness semantics and user cancellation; don't return a fake ready frame.
    private void ExtendNinaTimeout(CameraSession owner, double seconds)
    {
        RestoreNinaTimeout();
        if (profiles is null) return;
        var options = owner.Options;
        int retries = seconds <= options.MaximumRetryExposureSeconds ? options.MaxRetries : 0;
        int reads = seconds <= options.MaximumRetryExposureSeconds ? options.ReadyFrameDownloadRetries : 0;
        double attempt = seconds + options.ExposureGraceSeconds + options.CoolingTimeoutSeconds +
            (reads + 1) * (options.DownloadTimeoutSeconds + options.ReconnectDelaySeconds) +
            (2 * owner.Controls.Count + 10) * options.CommandTimeoutSeconds;
        int budget = (int)Math.Min(int.MaxValue, Math.Ceiling((retries + 1) * attempt));
        var settings = profiles.ActiveProfile.CameraSettings;
        if (settings.Timeout >= budget) return;
        timeoutSettings = settings;
        originalTimeout = settings.Timeout;
        extendedTimeout = budget;
        settings.Timeout = budget;
        Logger.Info($"ZWOgain temporarily extended NINA readiness timeout from {originalTimeout} to {budget} seconds for this capture");
    }
    private void RestoreNinaTimeout()
    {
        if (timeoutSettings is null) return;
        // A concurrent user edit takes precedence over our original value.
        if (timeoutSettings.Timeout == extendedTimeout) timeoutSettings.Timeout = originalTimeout;
        timeoutSettings = null;
    }
    public IList<string> SupportedActions => new List<string> { "ZwoGain.Diagnostics" };
    public string Action(string actionName, string actionParameters) => actionName == "ZwoGain.Diagnostics" ? System.Text.Json.JsonSerializer.Serialize(new { phase = session?.Phase, error = session?.LastError, sdkErrorCode = session?.LastSdkErrorCode, sdkExposureState = session?.LastSdkExposureState, serial = session?.Serial, sdk = session?.SdkVersion, backend = session?.Backend, sdkFallback = session?.UsingSdkFallback }) : throw new NotSupportedException();
    public string SendCommandString(string command, bool raw = true) => throw new NotSupportedException();
    public bool SendCommandBool(string command, bool raw = true) => throw new NotSupportedException();
    public void SendCommandBlind(string command, bool raw = true) => throw new NotSupportedException();
}
