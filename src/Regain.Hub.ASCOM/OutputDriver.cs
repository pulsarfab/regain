using System.Collections;
using System.Runtime.InteropServices;
using System.Text.Json;
using ASCOM.DeviceInterface;

namespace Regain.Hub.ASCOM;

/// Each COM object has one verified private IPC client. Equipment ownership,
/// command serialization and completion/failure state live in the Rust host.
/// A bound factory supplies the immutable selection; no first-output fallback.
[ComVisible(false)]
public abstract class OutputDriver : IDisposable
{
    private readonly object gate = new();
    private readonly HubSelection selection;
    private readonly HubNativeSession session;
    private readonly string executable;
    private readonly CancellationTokenSource lifetime = new();
    private Task? changing;
    private Guid operationEpoch = Guid.NewGuid();
    private bool disposed;
    private bool simulated;
    private string label;
    protected OutputDriver(HubSelection binding, string executable, string? workers)
    {
        selection = binding.Copy(); this.executable = executable;
        simulated = selection.Simulated; label = selection.Label;
        session = new HubNativeSession(executable, workers);
    }
    public string Name { get { lock (gate) return "PulsarFab regain " + label + (simulated ? " [SIMULATION]" : ""); } }
    public string Description => "Shared Regain " + selection.DeviceType + " output over private local IPC";
    public string DriverInfo => "PulsarFab regain shared Rust hub; native ASCOM output";
    public string DriverVersion => typeof(OutputDriver).Assembly.GetName().Version!.ToString();
    public abstract short InterfaceVersion { get; }
    public ArrayList SupportedActions => new();
    public string Action(string ActionName, string ActionParameters) => throw new global::ASCOM.MethodNotImplementedException("Action");
    public void CommandBlind(string Command, bool Raw) => throw new global::ASCOM.MethodNotImplementedException("CommandBlind");
    public bool CommandBool(string Command, bool Raw) => throw new global::ASCOM.MethodNotImplementedException("CommandBool");
    public string CommandString(string Command, bool Raw) => throw new global::ASCOM.MethodNotImplementedException("CommandString");
    public void SetupDialog()
    {
        lock (gate) CheckDisposed();
        // All exported COM calls run on the server's pumping STA. Setup edits
        // the same configuration as NINA; it cannot retarget this bound output.
        if (Thread.CurrentThread.GetApartmentState() != ApartmentState.STA)
            throw new global::ASCOM.DriverException("Hub setup requires the COM server STA");
        HubConfigurationWindow.Show(null, executable, selection.ConfigPath, selection.InstanceId);
    }
    public bool Connected {
        get {
            lock (gate) { CheckDisposed(); if (changing is { IsCompleted: false }) return false; CheckChange(); }
            return session.IsAttached && Get(new { member = "connected" }, "Connected").GetBoolean();
        }
        set => Change(value, false).GetAwaiter().GetResult();
    }
    public void Connect() => Change(true, true);
    public void Disconnect() => Change(false, true);
    public bool Connecting {
        get {
            lock (gate) { CheckDisposed(); if (changing is { IsCompleted: false }) return true; CheckChange(); }
            return session.IsAttached && Get(new { member = "connecting" }, "Connecting").GetBoolean();
        }
    }
    private Task Change(bool connected, bool asynchronous)
    {
        lock (gate) {
            CheckDisposed();
            if (changing is { IsCompleted: false }) throw new global::ASCOM.DriverException("Hub connection change is already pending");
            operationEpoch = Guid.NewGuid();
            // Starting the next explicit operation resets the previous local
            // completion error. Never replay connection changes after failure.
            changing = Task.Run(async () => {
                try {
                    if (!session.IsAttached) {
                        if (!connected) return;
                        var catalog = await session.AttachAsync(selection, lifetime.Token).ConfigureAwait(false);
                        lock (gate) {
                            simulated = catalog.GetProperty("simulated").GetBoolean();
                            label = catalog.GetProperty("label").GetString()!;
                        }
                    }
                    try {
                        session.RequireCapabilities("scalarDeviceState", "asyncOutputConnection");
                        if (selection.DeviceType == "switch") session.RequireCapabilities("switchAsyncContract");
                    } catch { session.Disconnect(); throw; }
                    var epoch = session.Epoch;
                    await session.RequestAsync(epoch, JsonSerializer.SerializeToElement(new { op = "changeConnection",
                        output = selection.OutputId, connected, asynchronous }), TimeSpan.FromSeconds(35), lifetime.Token).ConfigureAwait(false);
                } catch (Exception error) { throw Translate(error, connected ? "Connect" : "Disconnect", false, false); }
            });
            // Observe the task even if a client drops the object without ever
            // polling Connecting. The same exception remains in the task.
            _ = changing.ContinueWith(t => { _ = t.Exception; }, CancellationToken.None,
                TaskContinuationOptions.OnlyOnFaulted | TaskContinuationOptions.ExecuteSynchronously, TaskScheduler.Default);
            return changing;
        }
    }
    private void CheckChange() { if (changing?.IsFaulted == true) changing.GetAwaiter().GetResult(); }
    private void CheckDisposed() { if (disposed) throw new global::ASCOM.NotConnectedException("Hub output is disposed"); }
    protected JsonElement Get(object property, string member, bool method = false)
        => Request("get", property, member, false, method);
    protected void Put(object property, string member, bool method = true)
        => Request("put", property, member, true, method);
    private JsonElement Request(string operation, object property, string member, bool write, bool method)
    {
        Guid operationToken, sessionToken;
        lock (gate) {
            CheckDisposed(); CheckChange();
            if (changing is { IsCompleted: false }) throw new global::ASCOM.DriverException("Hub connection change is pending");
            operationToken = operationEpoch; sessionToken = session.Epoch;
        }
        if (!session.IsAttached) throw new global::ASCOM.NotConnectedException("Hub output is disconnected; connect explicitly");
        try {
            var value = session.RequestAsync(sessionToken, JsonSerializer.SerializeToElement(new { op = operation,
                output = selection.OutputId, property }), write ? TimeSpan.FromSeconds(35) : null, lifetime.Token).GetAwaiter().GetResult();
            lock (gate) {
                if (disposed || operationToken != operationEpoch) {
                    if (write) throw new global::ASCOM.DriverException("Hub connection changed while the command was in flight; it may have completed. Reconcile before another command.");
                    throw new global::ASCOM.NotConnectedException("Hub response belongs to a retired connection");
                }
            }
            return value;
        } catch (Exception error) { throw Translate(error, member, write, method); }
    }
    private static Exception Translate(Exception error, string member, bool write, bool method)
    {
        if (error is global::ASCOM.DriverException) return error;
        if (error is HubException hub) {
            if (hub.Remote is { } remote) {
                switch (remote.Code) {
                    case "disconnected": return new global::ASCOM.NotConnectedException("Hub output is disconnected");
                    case "invalidValue": return new global::ASCOM.InvalidValueException(member, "invalid", "configured limits");
                    case "unsupported": return method ? new global::ASCOM.MethodNotImplementedException(member)
                        : new global::ASCOM.PropertyNotImplementedException(member, write);
                    case "unavailable": return new global::ASCOM.ValueNotSetException("Hub " + member + " has no current reading; inspect source health");
                }
                return new global::ASCOM.DriverException("Hub " + member + " failed (" + remote.Code + "); inspect source health",
                    remote.UpstreamCode ?? unchecked((int)0x80040400));
            }
            return hub.Failure == HubFailure.Disconnected ? new global::ASCOM.NotConnectedException("Hub client connection closed")
                : new global::ASCOM.DriverException("Hub " + member + " failed (" + hub.Failure + "); reconcile before retrying");
        }
        return new global::ASCOM.DriverException("Hub " + member + " could not complete; inspect setup and reconnect explicitly");
    }
    public IStateValueCollection DeviceState {
        get {
            var states = Get(new { member = "deviceState" }, "DeviceState");
            var values = new List<StateValue>();
            foreach (var state in states.EnumerateArray()) {
                var value = state.GetProperty("Value");
                if (selection.DeviceType == "rotator") {
                    var property = state.GetProperty("Name").GetString() switch {
                        "IsMoving" => HubRotatorProperty.IsMoving, "Position" => HubRotatorProperty.Position,
                        "MechanicalPosition" => HubRotatorProperty.MechanicalPosition,
                        _ => throw new global::ASCOM.DriverException("Hub DeviceState contained an unknown rotator member")
                    };
                    try { HubRotatorProtocol.Validate(property, value); }
                    catch (HubException) { throw new global::ASCOM.DriverException("Hub DeviceState contained an invalid rotator reading"); }
                }
                if (selection.DeviceType == "filterwheel") {
                    if (state.GetProperty("Name").GetString() != "Position") throw new global::ASCOM.DriverException("Hub DeviceState contained an unknown filter wheel member");
                    try {HubFilterWheelProtocol.Validate(HubFilterWheelProperty.Position,value);}
                    catch (HubException) {throw new global::ASCOM.DriverException("Hub DeviceState contained an invalid filter wheel reading");}
                }
                object scalar = value.ValueKind switch {
                    JsonValueKind.True => true, JsonValueKind.False => false,
                    JsonValueKind.Number when selection.DeviceType == "focuser" && state.GetProperty("Name").GetString() == "Position" => value.GetInt32(),
                    JsonValueKind.Number when selection.DeviceType == "rotator" => value.GetSingle(),
                    JsonValueKind.Number when selection.DeviceType == "filterwheel" => value.GetInt16(),
                    JsonValueKind.Number => value.GetDouble(),
                    _ => throw new global::ASCOM.DriverException("Hub DeviceState contained an invalid scalar")
                };
                values.Add(new StateValue(state.GetProperty("Name").GetString(), scalar));
            }
            return new StateValueCollection(values);
        }
    }
    public void Dispose()
    {
        lock (gate) { if (disposed) return; disposed = true; operationEpoch = Guid.NewGuid(); }
        lifetime.Cancel(); session.Dispose(); GC.SuppressFinalize(this);
        // A pending operation still owns its token; do not dispose its CTS here.
    }
    ~OutputDriver() { session?.Dispose(); }
}
