using System.ComponentModel.Composition;
using System.Globalization;
using System.Text.Json;
using NINA.Core.Enum;
using NINA.Core.Model;
using NINA.Core.Model.Equipment;
using NINA.Core.Utility;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.ViewModel;
using NINA.Equipment.Model;
using NINA.Equipment.Utility;
using NINA.Image.ImageData;
using NINA.Image.Interfaces;
using NINA.Profile.Interfaces;
using Regain.Hub;

namespace Regain.NINA;

[Export(typeof(IEquipmentProvider))]
public sealed class HubCameraProvider : IEquipmentProvider<ICamera>
{
    private readonly IImageDataFactory images;
    private readonly IProfileService profiles;
    [ImportingConstructor]
    public HubCameraProvider(IImageDataFactory images, IProfileService profiles) { this.images = images; this.profiles = profiles; }
    public string Name => "PulsarFab regain";
    public IList<ICamera> GetEquipment() => HubEquipment.Choices<ICamera>("camera", binding => new HubCameraDevice(binding, images, profiles));
}

/// Native NINA presentation of the shared Rust acquisition controller. No SDK,
/// retry engine, HTTP listener or ASCOM output is required by this frontend.
public sealed class HubCameraDevice : HubTypedDevice, ICamera
{
    private readonly object captureGate = new();
    private readonly IImageDataFactory images;
    private readonly IProfileService? profiles;
    private CancellationTokenSource? lifetime;
    private Capture? capture;
    private ICameraSettings? timeoutSettings;
    private int originalTimeout, extendedTimeout;
    private short snapMode, normalMode;
    private sealed record Capture(Guid Epoch, Guid Acquisition, Task<JsonElement> Completed,
        int Width, int Height, short BinX, short BinY, int X, int Y, int Depth, SensorType Sensor, short BayerX, short BayerY,
        string CameraName, string CameraId);
    public HubCameraDevice(HubSelection? selection, IImageDataFactory images, IProfileService? profiles = null,
        string? executable = null, string? workers = null) : base("camera", selection, executable, workers)
    { this.images = images; this.profiles = profiles; }
    protected override Task<Action> Prepare(Guid epoch, Guid output, CancellationToken token)
    {
        Session.RequireCapabilities("cameraCaptureTiming");
        // Prepare runs outside HubDevice's connection gate. Its publication
        // callback must not take captureGate: capture methods read the base
        // connection context while holding that gate, so the reverse order
        // would deadlock a concurrent reconnect.
        lock (captureGate) { lifetime?.Dispose(); lifetime = new(); capture = null; RestoreTimeout(); }
        return Task.FromResult<Action>(() => { });
    }
    private JsonElement Read(HubCameraProperty property)
    {
        var context = RequireContext();
        return ReadTyped(context.Epoch, context.Binding.OutputId, HubCameraProtocol.Read(property),
            value => HubCameraProtocol.Validate(property, value), CancellationToken.None).GetAwaiter().GetResult();
    }
    private JsonElement Put(object property, CancellationToken token = default)
    {
        var context = RequireContext();
        try {
            return Session.RequestCameraAsync(context.Epoch, JsonSerializer.SerializeToElement(new { op = "put",
                output = context.Binding.OutputId, property }), token).GetAwaiter().GetResult();
        } catch { Failed(); throw; }
    }
    private void Set(HubCameraProperty property, object value) { Put(HubCameraProtocol.Setting(property, value)); RaiseAllPropertiesChanged(); }
    private bool Flag(HubCameraProperty property) => Read(property).GetBoolean();
    private int Integer(HubCameraProperty property) => Read(property).GetInt32();
    private short Short(HubCameraProperty property) => checked((short)Integer(property));
    private double Number(HubCameraProperty property) => Read(property).GetDouble();
    private bool Available(HubCameraProperty property) {
        try { Read(property); return true; }
        catch (HubException error) when (error.Remote?.Code == "unsupported") { return false; }
    }
    private double OptionalNumber(HubCameraProperty property) {
        try { return Number(property); }
        catch (HubException error) when (error.Remote?.Code == "unsupported") { return double.NaN; }
    }
    public bool HasShutter => Flag(HubCameraProperty.HasShutter);
    public string SensorName => Read(HubCameraProperty.SensorName).GetString()!;
    public SensorType SensorType => (SensorType)Integer(HubCameraProperty.SensorType);
    private bool Bayered => SensorType is SensorType.RGGB or SensorType.BGGR or SensorType.GRBG or SensorType.GBRG;
    public short BayerOffsetX => Bayered ? Short(HubCameraProperty.BayerOffsetX) : (short)0;
    public short BayerOffsetY => Bayered ? Short(HubCameraProperty.BayerOffsetY) : (short)0;
    public int CameraXSize => Integer(HubCameraProperty.CameraXSize);
    public int CameraYSize => Integer(HubCameraProperty.CameraYSize);
    public double PixelSizeX => Number(HubCameraProperty.PixelSizeX);
    public double PixelSizeY => Number(HubCameraProperty.PixelSizeY);
    public double ElectronsPerADU => OptionalNumber(HubCameraProperty.ElectronsPerAdu);
    public double ExposureMin => Number(HubCameraProperty.ExposureMin);
    public double ExposureMax => Number(HubCameraProperty.ExposureMax);
    public int BitDepth {
        get {
            var maximum = Integer(HubCameraProperty.MaxAdu);
            if (maximum <= 0) throw new IOException("Camera MaxADU cannot describe a scalar image");
            return 32 - System.Numerics.BitOperations.LeadingZeroCount((uint)maximum);
        }
    }
    public short BinX { get => Short(HubCameraProperty.BinX); set => SetBinning(value, Flag(HubCameraProperty.CanAsymmetricBin) ? BinY : value); }
    public short BinY { get => Short(HubCameraProperty.BinY); set => SetBinning(Flag(HubCameraProperty.CanAsymmetricBin) ? BinX : value, value); }
    public short MaxBinX => Short(HubCameraProperty.MaxBinX);
    public short MaxBinY => Short(HubCameraProperty.MaxBinY);
    public AsyncObservableCollection<BinningMode> BinningModes => new(Enumerable.Range(1, Math.Min(MaxBinX, MaxBinY)).Select(b => new BinningMode((short)b, (short)b)));
    private void ValidateBinning(short x, short y) {
        if (x <= 0 || y <= 0 || x > MaxBinX || y > MaxBinY || x != y && !Flag(HubCameraProperty.CanAsymmetricBin))
            throw new ArgumentOutOfRangeException(nameof(x));
    }
    public void SetBinning(short x, short y) {
        ValidateBinning(x, y);
        Set(HubCameraProperty.BinX, x); Set(HubCameraProperty.BinY, y);
    }
    public double Temperature => OptionalNumber(HubCameraProperty.CcdTemperature);
    public bool CanSetTemperature => Flag(HubCameraProperty.CanSetCcdTemperature);
    public double TemperatureSetPoint { get => OptionalNumber(HubCameraProperty.SetCcdTemperature); set => Set(HubCameraProperty.SetCcdTemperature, value); }
    public bool CoolerOn { get => Flag(HubCameraProperty.CoolerOn); set => Set(HubCameraProperty.CoolerOn, value); }
    public double CoolerPower => Flag(HubCameraProperty.CanGetCoolerPower) ? Number(HubCameraProperty.CoolerPower) : double.NaN;
    public bool CanGetGain => Available(HubCameraProperty.Gain);
    public bool CanSetGain => CanGetGain;
    public int Gain { get => Integer(HubCameraProperty.Gain); set => Set(HubCameraProperty.Gain, value); }
    public int GainMin => Available(HubCameraProperty.Gains) ? 0 : Integer(HubCameraProperty.GainMin);
    public int GainMax => Available(HubCameraProperty.Gains) ? Read(HubCameraProperty.Gains).GetArrayLength() - 1 : Integer(HubCameraProperty.GainMax);
    public IList<int> Gains => Available(HubCameraProperty.Gains) ? Enumerable.Range(0, Read(HubCameraProperty.Gains).GetArrayLength()).ToList() : [];
    public bool CanSetOffset => Available(HubCameraProperty.Offset);
    public int Offset { get => Integer(HubCameraProperty.Offset); set => Set(HubCameraProperty.Offset, value); }
    public int OffsetMin => Available(HubCameraProperty.Offsets) ? 0 : Integer(HubCameraProperty.OffsetMin);
    public int OffsetMax => Available(HubCameraProperty.Offsets) ? Read(HubCameraProperty.Offsets).GetArrayLength() - 1 : Integer(HubCameraProperty.OffsetMax);
    public IList<string> ReadoutModes => Read(HubCameraProperty.ReadoutModes).EnumerateArray().Select(value => value.GetString()!).ToList();
    public short ReadoutMode { get => Short(HubCameraProperty.ReadoutMode); set => Set(HubCameraProperty.ReadoutMode, value); }
    public short ReadoutModeForSnapImages { get => snapMode; set { if (value < 0 || value >= ReadoutModes.Count) throw new ArgumentOutOfRangeException(nameof(value)); snapMode = value; } }
    public short ReadoutModeForNormalImages { get => normalMode; set { if (value < 0 || value >= ReadoutModes.Count) throw new ArgumentOutOfRangeException(nameof(value)); normalMode = value; } }
    public CameraStates CameraState {
        get {
            if (!Connected) return CameraStates.NoState;
            lock (captureGate) { if (capture?.Completed.IsFaulted == true) return CameraStates.Error; }
            return (CameraStates)Integer(HubCameraProperty.CameraState);
        }
    }
    public bool CanSubSample => true;
    public bool EnableSubSample { get; set; }
    public int SubSampleX { get; set; }
    public int SubSampleY { get; set; }
    public int SubSampleWidth { get; set; }
    public int SubSampleHeight { get; set; }
    public void UpdateSubSampleArea() { }
    // The generic camera contract does not promise these vendor extensions.
    public bool HasDewHeater => false;
    public bool DewHeaterOn { get => false; set => throw new NotSupportedException(); }
    public bool CanSetUSBLimit => false;
    public int USBLimit { get => -1; set => throw new NotSupportedException(); }
    public int USBLimitMin => -1;
    public int USBLimitMax => -1;
    public int USBLimitStep => 1;
    public bool HasBattery => false;
    public int BatteryLevel => -1;
    public bool CanShowLiveView => false;
    public bool LiveViewEnabled => false;
    public void StartLiveView(CaptureSequence sequence) => throw new NotSupportedException();
    public Task<IExposureData> DownloadLiveView(CancellationToken token) => throw new NotSupportedException();
    public void StopLiveView() { }

    public void StartExposure(CaptureSequence sequence)
    {
        ArgumentNullException.ThrowIfNull(sequence);
        ArgumentNullException.ThrowIfNull(sequence.Binning);
        lock (captureGate) {
            if (capture is { Completed.IsCompleted: false }) throw new InvalidOperationException("A camera acquisition is already active");
            var context = RequireContext();
            var token = lifetime?.Token ?? throw new InvalidOperationException("Camera lifetime is unavailable");
            var timing = Session.GetCameraCaptureTimingAsync(context.Epoch, sequence.ExposureTime, token).GetAwaiter().GetResult();
            var sensor = SensorType;
            if (sensor is not (SensorType.Monochrome or SensorType.RGGB or SensorType.BGGR or SensorType.GRBG or SensorType.GBRG))
                throw new NotSupportedException("NINA's scalar hub pipeline cannot represent this camera's color layout");
            var bx = sequence.Binning.X; var by = sequence.Binning.Y;
            ValidateBinning(bx, by);
            var cameraWidth = CameraXSize; var cameraHeight = CameraYSize;
            var width = (EnableSubSample ? SubSampleWidth : cameraWidth) / bx;
            var height = (EnableSubSample ? SubSampleHeight : cameraHeight) / by;
            var x = EnableSubSample ? SubSampleX / bx : 0; var y = EnableSubSample ? SubSampleY / by : 0;
            if (width <= 0 || height <= 0 || x < 0 || y < 0 || (long)(x + (long)width) * bx > cameraWidth
                || (long)(y + (long)height) * by > cameraHeight || EnableSubSample && (SubSampleX < 0 || SubSampleY < 0))
                throw new ArgumentOutOfRangeException(nameof(sequence));
            // Shrink the previous rectangle before bin changes. Admission still
            // belongs to Rust/the source; do not invent device alignment rules.
            Put(HubCameraProtocol.Setting(HubCameraProperty.NumX, 1), token);
            Put(HubCameraProtocol.Setting(HubCameraProperty.NumY, 1), token);
            Put(HubCameraProtocol.Setting(HubCameraProperty.StartX, 0), token);
            Put(HubCameraProtocol.Setting(HubCameraProperty.StartY, 0), token);
            SetBinning(sequence.Binning.X, sequence.Binning.Y);
            Put(HubCameraProtocol.Setting(HubCameraProperty.StartX, x), token); Put(HubCameraProtocol.Setting(HubCameraProperty.StartY, y), token);
            Put(HubCameraProtocol.Setting(HubCameraProperty.NumX, width), token); Put(HubCameraProtocol.Setting(HubCameraProperty.NumY, height), token);
            if (sequence.Gain >= 0 && CanSetGain) Gain = sequence.Gain;
            if (sequence.Offset >= 0 && CanSetOffset) Offset = sequence.Offset;
            var depth = BitDepth; var bayerX = BayerOffsetX; var bayerY = BayerOffsetY;
            // Freeze the upstream model separately from the Hub chooser label.
            // A completed frame must retain its identity across profile edits.
            // NINA's FITS string writer truncates the long chooser identity.
            // The immutable output UUID fits completely in CAMERAID.
            var cameraName = SensorName; var cameraId = context.Binding.OutputId.ToString();
            RestoreTimeout(); ExtendTimeout(timing);
            try {
                var light = !sequence.IsDarkSequence();
                var accepted = Put(HubCameraProtocol.Start(sequence.ExposureTime, light), token).GetGuid();
                if (accepted == Guid.Empty) throw new IOException("Camera returned an empty acquisition identity");
                var current = new Capture(context.Epoch, accepted, AwaitCompleted(context.Epoch, accepted, timing, light, token), width, height, bx, by, x, y, depth, sensor, bayerX, bayerY, cameraName, cameraId);
                capture = current;
                _ = current.Completed.ContinueWith(failed => {
                    _ = failed.Exception; // Observe failures even if NINA never waits.
                    lock (captureGate) { if (ReferenceEquals(current, capture)) RestoreTimeout(); }
                }, CancellationToken.None, TaskContinuationOptions.OnlyOnFaulted | TaskContinuationOptions.ExecuteSynchronously, TaskScheduler.Default);
            } catch { RestoreTimeout(); throw; }
        }
        RaiseAllPropertiesChanged();
    }
    private async Task<JsonElement> AwaitCompleted(Guid epoch, Guid acquisition, HubCameraCaptureTiming timing, bool light, CancellationToken lifetimeToken)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(lifetimeToken);
        deadline.CancelAfter(timing.Completion + TimeSpan.FromSeconds(5));
        while (true) {
            var status = await Session.ReadCameraAcquisitionAsync(epoch, deadline.Token).ConfigureAwait(false);
            if (status.GetProperty("phase").GetString() == "uncertain" || status.GetProperty("error").ValueKind != JsonValueKind.Null)
                throw new IOException("Camera acquisition failed or has an uncertain outcome; inspect hub source health");
            if (status.GetProperty("imageReady").GetBoolean()) {
                var completed = status.GetProperty("completed");
                if (completed.GetProperty("acquisition").GetGuid() != acquisition)
                    throw new IOException("The accepted camera acquisition was replaced; no new exposure was requested");
                if (completed.GetProperty("source").GetGuid() != timing.Source
                    || completed.GetProperty("generation").GetGuid() != status.GetProperty("generation").GetGuid()
                    || completed.GetProperty("request").GetProperty("durationSeconds").GetDouble() != timing.DurationSeconds
                    || completed.GetProperty("request").GetProperty("light").GetBoolean() != light)
                    throw new IOException("Completed camera identity or request differs from the accepted acquisition");
                return completed.Clone();
            }
            if (status.GetProperty("acquisition").ValueKind == JsonValueKind.Null || status.GetProperty("acquisition").GetGuid() != acquisition)
                throw new IOException("The accepted camera acquisition is no longer active or completed");
            await Task.Delay(100, deadline.Token).ConfigureAwait(false);
        }
    }
    private Capture CurrentCapture() {
        lock (captureGate) return capture ?? throw new InvalidOperationException("No camera acquisition was started");
    }
    private void RequireCaptureEpoch(Capture current) {
        if (ReadContext is not { } context || context.Epoch != current.Epoch)
            throw new InvalidOperationException("Camera acquisition belongs to a retired NINA connection");
    }
    public async Task WaitUntilExposureIsReady(CancellationToken token)
    {
        var current = CurrentCapture();
        try { await current.Completed.WaitAsync(token).ConfigureAwait(false); RequireCaptureEpoch(current); }
        catch { lock (captureGate) { if (ReferenceEquals(current, capture)) RestoreTimeout(); } throw; }
    }
    public async Task<IExposureData> DownloadExposure(CancellationToken token)
    {
        var current = CurrentCapture();
        try {
            var completed = await current.Completed.WaitAsync(token).ConfigureAwait(false);
            var geometry = completed.GetProperty("geometry");
            if (geometry.GetProperty("width").GetInt32() != current.Width || geometry.GetProperty("height").GetInt32() != current.Height
                || geometry.GetProperty("binX").GetInt32() != current.BinX || geometry.GetProperty("binY").GetInt32() != current.BinY
                || geometry.GetProperty("startX").GetInt32() != current.X || geometry.GetProperty("startY").GetInt32() != current.Y)
                throw new IOException("Accepted camera geometry differs from the requested NINA frame");
            var metadata = HubCameraFrames.Metadata(geometry, completed.GetProperty("exposure"), current.Sensor, current.BayerX, current.BayerY);
            metadata.Camera.Name = current.CameraName;
            metadata.Camera.Id = current.CameraId;
            var bayered = current.Sensor is SensorType.RGGB or SensorType.BGGR or SensorType.GRBG or SensorType.GBRG;
            using var image = await Session.DownloadCameraImageAsync(current.Epoch, HubImageBudget.Shared, current.Acquisition, token).ConfigureAwait(false);
            if (image.Descriptor.Width != current.Width || image.Descriptor.Height != current.Height) throw new IOException("Camera image dimensions differ from frozen metadata");
            var signed = current.Depth > 16 || profiles?.ActiveProfile.CameraSettings.ASCOMCreate32BitData == true;
            var pixels = HubCameraArrays.RowMajorIntegers(image, signed, cancellation: token);
            RequireCaptureEpoch(current);
            return new HubCameraExposureData(images, pixels, current.Width, current.Height, current.Depth, bayered, metadata);
        } finally { lock (captureGate) { if (ReferenceEquals(current, capture)) RestoreTimeout(); } }
    }
    public void StopExposure() { Put(new { member = "stopExposure" }); }
    public void AbortExposure() { Put(new { member = "abortExposure" }); }
    private void ExtendTimeout(HubCameraCaptureTiming timing) {
        if (profiles is null) return;
        var seconds = checked((int)Math.Ceiling((timing.Completion + HubCameraImages.MaximumTransferTime + TimeSpan.FromSeconds(5)).TotalSeconds));
        var settings = profiles.ActiveProfile.CameraSettings;
        if (settings.Timeout >= seconds) return;
        timeoutSettings = settings; originalTimeout = settings.Timeout; extendedTimeout = seconds; settings.Timeout = seconds;
    }
    private void RestoreTimeout() {
        if (timeoutSettings is null) return;
        if (timeoutSettings.Timeout == extendedTimeout) timeoutSettings.Timeout = originalTimeout;
        timeoutSettings = null;
    }
    public override void Disconnect() {
        // Cancellation is observational: the host retains accepted work for its
        // owner and other clients. Never implicitly Stop/Abort during teardown.
        try { lifetime?.Cancel(); } catch (ObjectDisposedException) { }
        base.Disconnect();
        lock (captureGate) { RestoreTimeout(); capture = null; lifetime?.Dispose(); lifetime = null; }
    }
}

internal sealed class HubCameraExposureData : IExposureData
{
    private readonly Lazy<IImageData> converted;
    public int BitDepth { get; }
    public ImageMetaData MetaData { get; }
    internal Array Pixels { get; }
    internal int Width { get; }
    internal int Height { get; }
    internal HubCameraExposureData(IImageDataFactory factory, Array pixels, int width, int height, int depth, bool bayer, ImageMetaData metadata) {
        Pixels = pixels; Width = width; Height = height; BitDepth = depth; MetaData = metadata;
        converted = new(() => factory.CreateBaseImageData(pixels is int[] integers ? new ImageArrayInt(integers) : new ImageArray((ushort[])pixels),
            width, height, depth, bayer, metadata));
    }
    public Task<IImageData> ToImageData(IProgress<ApplicationStatus> progress, CancellationToken token) {
        token.ThrowIfCancellationRequested(); return Task.FromResult(converted.Value);
    }
}
