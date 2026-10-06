using System.ComponentModel.Composition;
using System.Text.Json;
using NINA.Core.Utility;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.ViewModel;
using Regain.Hub;
using Regain.Rotator;

namespace Regain.NINA;

internal static class HubEquipment
{
    internal static string Executable => RegainPaths.EnvironmentVariable("REGAIN_HUB_HOST") ?? Path.Combine(CameraProvider.DirectoryPath, "regain-alpaca.exe");
    internal static HubSelectionStore Store => new(RegainPaths.EnvironmentVariable("REGAIN_HUB_BINDINGS") ?? RegainPaths.Profile("hub-frontends.json"));
    // Enumeration reads saved identities only. Discovering or connecting sources
    // belongs to explicit setup/connect, never to NINA's provider enumeration.
    internal static IList<T> Choices<T>(string type, Func<HubSelection?, T> create, HubSelectionStore? store = null)
    {
        var choices = new List<T>();
        try { choices.AddRange((store ?? Store).Load().Bindings.Where(b => b.DeviceType == type).Select(b => create(b.Copy()))); }
        catch { Logger.Error("Regain hub selections could not be read; open hub setup to resolve the saved file."); }
        choices.Add(create(null)); return choices;
    }
}
[Export(typeof(IEquipmentProvider))]
public sealed class HubSwitchProvider : IEquipmentProvider<ISwitchHub>
{
    public string Name => "PulsarFab regain";
    public IList<ISwitchHub> GetEquipment() => HubEquipment.Choices<ISwitchHub>("switch", b => new HubSwitchDevice(b));
}
[Export(typeof(IEquipmentProvider))]
public sealed class HubSafetyProvider : IEquipmentProvider<ISafetyMonitor>
{
    public string Name => "PulsarFab regain";
    public IList<ISafetyMonitor> GetEquipment() => HubEquipment.Choices<ISafetyMonitor>("safetymonitor", b => new HubSafetyDevice(b));
}
[Export(typeof(IEquipmentProvider))]
public sealed class HubWeatherProvider : IEquipmentProvider<IWeatherData>
{
    public string Name => "PulsarFab regain";
    public IList<IWeatherData> GetEquipment() => HubEquipment.Choices<IWeatherData>("observingconditions", b => new HubWeatherDevice(b));
}

public abstract class HubDevice : BaseINPC, IDevice, IDisposable
{
    private readonly object gate = new();
    protected readonly HubNativeSession Session;
    private HubSelection? selection;
    private readonly string type;
    private bool ready, connecting, disposed;
    private int disconnecting;
    private Guid connectedEpoch;
    private CancellationTokenSource? connectionAttempt;
    protected HubDevice(string type, HubSelection? selection, string? executable = null, string? workers = null)
    {
        if (selection is not null && selection.DeviceType != type) throw new ArgumentException("Hub selection has a different device class");
        this.type = type; this.selection = selection?.Copy(); Session = new(executable ?? HubEquipment.Executable, workers);
    }
    public string Id { get { lock (gate) return selection?.Id ?? "PulsarFab.Regain.Hub.Configure." + type; } }
    public string Name { get { lock (gate) return selection is null ? "PulsarFab regain hub — configure " + type :
        "PulsarFab regain hub: " + selection.Label + (selection.Simulated ? " [SIMULATION]" : ""); } }
    public string DisplayName => Name;
    public string Category => "PulsarFab regain";
    public string Description => "Shared Regain " + type + " output over private local IPC";
    public string DriverInfo => "PulsarFab regain shared Rust hub; native NINA output";
    public string DriverVersion => typeof(HubDevice).Assembly.GetName().Version!.ToString();
    public bool HasSetupDialog => true;
    public virtual bool Connected { get { lock (gate) return ready && !disposed && Session.Connected; } }
    public string LastError { get; private set; } = "";
    protected void Failed() { LastError = "Hub value unavailable; inspect source health in hub setup."; }
    protected JsonElement Get(Guid epoch, Guid output, object property) => Session.RequestAsync(epoch,
        JsonSerializer.SerializeToElement(new { op = "get", output, property })).GetAwaiter().GetResult();
    protected HubSelection? Selection { get { lock (gate) return selection?.Copy(); } }
    protected (HubSelection Binding, Guid Epoch)? ReadContext {
        get { lock (gate) return ready && !disposed && Session.Connected && Session.Epoch == connectedEpoch && selection is not null
            ? (selection.Copy(), connectedEpoch) : null; }
    }
    public async Task<bool> Connect(CancellationToken token)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(token);
        deadline.CancelAfter(TimeSpan.FromSeconds(45));
        HubSelection binding;
        lock (gate) {
            if (disposed) throw new ObjectDisposedException(nameof(HubDevice));
            if (connecting || disconnecting != 0 || (ready && Session.Connected)) throw new InvalidOperationException("Hub device is connected or changing connection");
            binding = selection?.Copy() ?? throw new InvalidOperationException("Open hub setup and select an output first");
            connecting = true; ready = false; connectionAttempt = deadline;
        }
        try {
            // A closed transport requires an explicit Connect; it is never
            // reattached by getters or writable switch calls.
            Session.Disconnect();
            var descriptor = await Session.ConnectAsync(binding, deadline.Token).ConfigureAwait(false);
            var epoch = Session.Epoch;
            var publish = await Prepare(epoch, binding.OutputId, deadline.Token).ConfigureAwait(false);
            lock (gate) {
                if (disposed || !Session.Connected || Session.Epoch != epoch || deadline.IsCancellationRequested)
                    throw new OperationCanceledException(deadline.Token);
                selection!.Label = descriptor.GetProperty("label").GetString()!;
                selection.Simulated = descriptor.GetProperty("simulated").GetBoolean();
                publish(); connectedEpoch = epoch; ready = true; LastError = "";
            }
            RaiseAllPropertiesChanged(); return true;
        } catch {
            Session.Disconnect(); lock (gate) ready = false;
            Failed(); RaiseAllPropertiesChanged(); throw;
        } finally { lock (gate) { connecting = false; connectionAttempt = null; } }
    }
    protected virtual Task<Action> Prepare(Guid epoch, Guid output, CancellationToken token) => Task.FromResult<Action>(() => { });
    public void Disconnect()
    {
        CancellationTokenSource? attempt;
        lock (gate) { ready = false; disconnecting++; attempt = connectionAttempt; }
        try {
            try { attempt?.Cancel(); } catch (ObjectDisposedException) { }
            Session.Disconnect();
        } finally { lock (gate) disconnecting--; RaiseAllPropertiesChanged(); }
    }
    public void SetupDialog()
    {
        lock (gate) {
            if (connecting || disconnecting != 0 || (ready && Session.Connected)) throw new InvalidOperationException("Disconnect this NINA device before changing its saved output");
            if (disposed) throw new ObjectDisposedException(nameof(HubDevice));
        }
        var chosen = HubSelectionWindow.Select(HubEquipment.Executable, HubEquipment.Store, type, Selection);
        if (chosen is not null) {
            lock (gate) {
                if (connecting || disconnecting != 0 || (ready && Session.Connected) || disposed) throw new InvalidOperationException("The device connection changed while setup was open");
                selection = chosen.Copy();
            }
        }
        RaiseAllPropertiesChanged();
    }
    public IList<string> SupportedActions => [];
    public string Action(string actionName, string actionParameters) => throw new NotSupportedException();
    public string SendCommandString(string command, bool raw = true) => throw new NotSupportedException();
    public bool SendCommandBool(string command, bool raw = true) => throw new NotSupportedException();
    public void SendCommandBlind(string command, bool raw = true) => throw new NotSupportedException();
    public void Dispose() { lock (gate) disposed = true; try { Disconnect(); } finally { Session.Dispose(); } }
}

public sealed class HubSafetyDevice : HubDevice, ISafetyMonitor
{
    public HubSafetyDevice(HubSelection? selection, string? executable = null, string? workers = null) : base("safetymonitor", selection, executable, workers) { }
    public bool IsSafe {
        get {
            if (ReadContext is not { } context) return false;
            try { return Get(context.Epoch, context.Binding.OutputId, new { member = "isSafe" }).GetBoolean(); }
            catch { Failed(); return false; }
        }
    }
}
public sealed class HubWeatherDevice : HubDevice, IWeatherData
{
    public HubWeatherDevice(HubSelection? selection, string? executable = null, string? workers = null) : base("observingconditions", selection, executable, workers) { }
    private double Read(string metric)
    {
        if (ReadContext is not { } context) return double.NaN;
        try {
            var value = Get(context.Epoch, context.Binding.OutputId, new { member = "measurement", metric }).GetProperty("value").GetDouble();
            return double.IsFinite(value) ? value : double.NaN;
        } catch { Failed(); return double.NaN; }
    }
    public double AveragePeriod {
        get {
            if (ReadContext is not { } context) return double.NaN;
            try { return Get(context.Epoch, context.Binding.OutputId, new { member = "averagePeriod" }).GetDouble(); }
            catch { Failed(); return double.NaN; }
        }
    }
    public double CloudCover => Read("cloudcover");
    public double DewPoint => Read("dewpoint");
    public double Humidity => Read("humidity");
    public double Pressure => Read("pressure");
    public double RainRate => Read("rainrate");
    public double SkyBrightness => Read("skybrightness");
    public double SkyQuality => Read("skyquality");
    public double SkyTemperature => Read("skytemperature");
    public double StarFWHM => Read("starfwhm");
    public double Temperature => Read("temperature");
    public double WindDirection => Read("winddirection");
    public double WindGust => Read("windgust");
    public double WindSpeed => Read("windspeed");
}

public sealed class HubSwitchDevice : HubDevice, ISwitchHub
{
    private volatile ICollection<ISwitch> switches = Array.AsReadOnly(Array.Empty<ISwitch>());
    public HubSwitchDevice(HubSelection? selection, string? executable = null, string? workers = null) : base("switch", selection, executable, workers) { }
    public ICollection<ISwitch> Switches => switches;
    protected override async Task<Action> Prepare(Guid epoch, Guid output, CancellationToken token)
    {
        async Task<JsonElement> Read(object property) => await Session.RequestAsync(epoch,
            JsonSerializer.SerializeToElement(new { op = "get", output, property }), TimeSpan.FromSeconds(35), token).ConfigureAwait(false);
        var count = (await Read(new { member = "maxSwitch" }).ConfigureAwait(false)).GetInt32();
        if (count is < 0 or > 1024) throw new InvalidOperationException("Unsupported hub switch slot count");
        var channels = new List<ISwitch>();
        for (short id = 0; id < count; id++) {
            var name = (await Read(new { member = "getSwitchName", id }).ConfigureAwait(false)).GetString()!;
            var description = (await Read(new { member = "getSwitchDescription", id }).ConfigureAwait(false)).GetString()!;
            bool writable;
            try { writable = (await Read(new { member = "canWrite", id }).ConfigureAwait(false)).GetBoolean(); }
            catch (HubException error) when (error.Failure == HubFailure.Remote) { writable = false; description += "; writable capability unavailable — reconnect after resolving source health"; }
            if (writable) {
                var minimum = (await Read(new { member = "minSwitchValue", id }).ConfigureAwait(false)).GetDouble();
                var maximum = (await Read(new { member = "maxSwitchValue", id }).ConfigureAwait(false)).GetDouble();
                var step = (await Read(new { member = "switchStep", id }).ConfigureAwait(false)).GetDouble();
                channels.Add(new HubWritableSwitch(Session, epoch, output, id, name, description, minimum, maximum, step));
            } else channels.Add(new HubSwitch(Session, epoch, output, id, name, description));
            // Seed the UI from cached readings; failed/retired channels remain
            // unavailable. Initializing a target never sends a write.
            if (channels[channels.Count - 1].Poll() && channels[channels.Count - 1] is IWritableSwitch write)
                write.TargetValue = write.Value;
        }
        var frozen = Array.AsReadOnly(channels.ToArray());
        return () => switches = frozen;
    }
}

public class HubSwitch : BaseINPC, ISwitch
{
    protected readonly HubNativeSession Session;
    protected readonly Guid Epoch, Output;
    protected readonly object SampleGate = new();
    private double value = double.NaN;
    protected long SampleVersion;
    protected bool Writing;
    protected bool Valid;
    internal HubSwitch(HubNativeSession session, Guid epoch, Guid output, short id, string name, string description)
    { Session = session; Epoch = epoch; Output = output; Id = id; Name = name; Description = description; }
    public short Id { get; }
    public string Name { get; }
    public string Description { get; }
    public virtual double Value { get { lock (SampleGate) return Valid && Session.Connected && Session.Epoch == Epoch ? value : double.NaN; } }
    public bool Poll()
    {
        long version;
        lock (SampleGate) {
            if (Writing) return false;
            version = SampleVersion;
        }
        double next = double.NaN;
        try {
            next = Session.RequestAsync(Epoch, JsonSerializer.SerializeToElement(new { op = "get", output = Output,
                property = new { member = "getSwitchValue", id = Id } })).GetAwaiter().GetResult().GetDouble();
        } catch { }
        bool valid;
        lock (SampleGate) {
            // A read started before a write cannot restore the old sample.
            // I/O never holds the channel lock or delays unrelated gauges.
            if (Writing || SampleVersion != version) return false;
            valid = Valid = double.IsFinite(next); value = valid ? next : double.NaN;
        }
        RaisePropertyChanged(nameof(Value)); return valid;
    }
}
public sealed class HubWritableSwitch : HubSwitch, IWritableSwitch
{
    private double target;
    internal HubWritableSwitch(HubNativeSession session, Guid epoch, Guid output, short id, string name, string description,
        double minimum, double maximum, double step) : base(session, epoch, output, id, name, description)
    {
        if (!double.IsFinite(minimum) || !double.IsFinite(maximum) || !double.IsFinite(step) || maximum < minimum || step <= 0)
            throw new InvalidOperationException("Invalid writable switch range");
        Minimum = minimum; Maximum = maximum; StepSize = step; target = minimum;
    }
    public double Minimum { get; }
    public double Maximum { get; }
    public double StepSize { get; }
    public double TargetValue {
        get { lock (SampleGate) return target; }
        set {
            if (!double.IsFinite(value) || value < Minimum || value > Maximum) throw new ArgumentOutOfRangeException(nameof(value));
            // Match the Rust configured grid, anchored at minimum with half-way
            // steps rounded away from zero. The host still authorizes each write.
            var next = Math.Clamp(Minimum + Math.Round((value - Minimum) / StepSize, MidpointRounding.AwayFromZero) * StepSize, Minimum, Maximum);
            if (!double.IsFinite(next)) throw new ArgumentOutOfRangeException(nameof(value));
            lock (SampleGate) target = next;
            RaisePropertyChanged();
        }
    }
    public override double Value {
        get {
            var value = base.Value;
            // NINA's write completion loop treats NaN comparisons as success.
            // Invalid writable readback must throw instead of implying completion.
            return double.IsFinite(value) ? value : throw new InvalidOperationException("Writable switch readback is unavailable; completion is unknown");
        }
    }
    public void SetValue()
    {
        double requested;
        lock (SampleGate) {
            if (Writing) throw new InvalidOperationException("A write is already pending for this channel");
            Valid = false; SampleVersion++; Writing = true; requested = target;
        }
        try {
            Session.RequestAsync(Epoch, JsonSerializer.SerializeToElement(new { op = "put", output = Output,
                property = new { member = "setSwitchValue", id = Id, value = requested } }), TimeSpan.FromSeconds(35)).GetAwaiter().GetResult();
        } finally {
            lock (SampleGate) Writing = false;
            RaisePropertyChanged(nameof(Value));
        }
    }
}
