using System.Globalization;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Threading;
using System.Security.Principal;
using Microsoft.Win32;

namespace Regain.Hub.ASCOM;

/// One object, exclusively called on the worker's message-pumping STA. No driver
/// reflection member is accepted directly from a caller; tables below whitelist
/// supported typed members. Camera arrays require a separate protocol.
internal sealed partial class ImportDriver {
    private enum Phase { Activate, Version, Check, Open, WaitOpen, Verify, Ready, WaitClose, Closed, Failed }
    private readonly Options options;
    private readonly int sta = Thread.CurrentThread.ManagedThreadId;
    private object? driver;
    private Phase phase;
    private short? version;
    private bool modern;
    private bool owns;
    private bool uncertain;
    private bool writeUncertain;
    private long lastId;
    internal ComCameraImage? PendingImage { get; private set; }
    internal ComCameraImage? TakeImage() {
        AssertSta();
        var image = PendingImage;
        PendingImage = null;
        return image;
    }

    public ImportDriver(Options options) { this.options = options; }
    private static Guid RegisteredClass(string progId)
    {
        using var identity = WindowsIdentity.GetCurrent();
        var elevated = new WindowsPrincipal(identity).IsInRole(WindowsBuiltInRole.Administrator);
        // Elevated COM ignores per-user registrations, even though HKCR can
        // show their merged values. Otherwise use the actual merged class view.
        using var root = RegistryKey.OpenBaseKey(elevated ? RegistryHive.LocalMachine : RegistryHive.ClassesRoot, RegistryView.Default);
        using var key = root.OpenSubKey((elevated ? "Software\\Classes\\" : "") + progId + "\\CLSID");
        if (key?.GetValue(null) is not string text || !Guid.TryParse(text, out var clsid) || clsid == Guid.Empty)
            throw new ConnectionFault();
        return clsid;
    }

    public object Execute(Request request) {
        AssertSta();
        if (request.Id <= lastId) throw new InvalidOperationException(); // terminal replay/ordering violation
        lastId = request.Id;
        PendingImage = null;
        object? value = null;
        object? error = null;
        var mutation = request.Operation is "write" or "refresh";
        try {
            switch (request.Operation) {
                case "connectStep": value = ConnectStep(); break;
                case "disconnectStep": value = DisconnectStep(); break;
                case "read": RequireReady(); value = Read(request.Member, request.Parameters); break;
                case "image":
                    RequireReady();
                    if (options.DeviceType != "camera") throw new Unsupported();
                    if (!Boolean(Get("ImageReady"))) throw new BadValue();
                    var pixels = Get("ImageArray"); // Exactly one upstream getter; never recapture.
                    try { PendingImage = new ComCameraImage(pixels); }
                    catch (ArgumentException) { throw new BadValue(); }
                    value = PendingImage.Descriptor; break;
                case "write":
                    RequireReady();
                    if (writeUncertain) throw new UncertainCommand();
                    value = Write(request.Member, request.Parameters); break;
                case "refresh":
                    RequireReady();
                    if (writeUncertain) throw new UncertainCommand();
                    if (options.DeviceType != "observingconditions") throw new Unsupported();
                    Call("Refresh"); break;
            }
        } catch (Exception exception) {
            var cause = Unwrap(exception);
            var kind = Classify(cause, mutation);
            if (mutation && kind == "uncertain") writeUncertain = true;
            if (request.Operation == "connectStep" || request.Operation == "disconnectStep") {
                // Once a connection change has failed, no subsequent RPC can
                // retry it or turn this object into a different ownership mode.
                if (uncertain && kind is not ("invalidValue" or "unsupported")) kind = "uncertain";
                else if (kind is "invalidValue" or "unsupported") uncertain = false;
                phase = Phase.Failed;
            }
            error = new { kind, code = cause is InvalidInput or Unsupported or BadValue or NotConnected or ConnectionFault or UncertainCommand ? (int?)null : cause.HResult };
        }
        return new { protocol = 1, id = request.Id, value, error, connection = Info() };
    }

    private object Info() => new {
        deviceType = options.DeviceType, interfaceVersion = version,
        method = modern ? "async" : "legacy", ownsConnection = owns,
        uncertain, ready = phase == Phase.Ready
    };

    private bool ConnectStep() {
        if (phase == Phase.Failed || uncertain) throw new ConnectionFault();
        switch (phase) {
            case Phase.Activate:
                var classId = RegisteredClass(options.ProgId);
                if (options.DeniedClasses.Contains(classId)) throw new InvalidInput();
                // Activate the checked CLSID, not another lookup of a mutable
                // ProgID. Managed Type.GUID may describe the managed class
                // rather than the alias's actual registry binding.
                var type = Type.GetTypeFromCLSID(classId, throwOnError: true) ?? throw new ConnectionFault();
                driver = Activator.CreateInstance(type) ?? throw new ConnectionFault();
                phase = Phase.Version; break;
            case Phase.Version:
                try { version = Integer(Get("InterfaceVersion"), 1, short.MaxValue); }
                catch (Exception error) when (Classify(Unwrap(error), false) == "unsupported") { version = null; }
                modern = version >= (options.DeviceType switch { "observingconditions" or "covercalibrator" => 2, "focuser" or "rotator" or "camera" => 4, _ => 3 });
                phase = Phase.Check; break;
            case Phase.Check:
                var connected = Boolean(Get("Connected"));
                if (connected && (!options.Managed || !modern)) phase = Phase.Ready;
                else if (!options.Managed) throw new NotConnected();
                else phase = Phase.Open;
                break;
            case Phase.Open:
                uncertain = true;
                if (modern) Call("Connect"); else Set("Connected", true);
                owns = true;
                uncertain = false;
                phase = modern ? Phase.WaitOpen : Phase.Verify; break;
            case Phase.WaitOpen:
                if (!Boolean(Get("Connecting"))) phase = Phase.Verify;
                break;
            case Phase.Verify:
                if (!Boolean(Get("Connected"))) throw new ConnectionFault();
                phase = Phase.Ready; break;
            case Phase.Ready: return true;
            default: throw new ConnectionFault();
        }
        return phase == Phase.Ready;
    }

    private bool DisconnectStep() {
        if (uncertain) throw new ConnectionFault();
        if (phase == Phase.WaitClose) {
            if (!Boolean(Get("Connecting"))) phase = Phase.Closed;
            return phase == Phase.Closed;
        }
        if (owns) {
            // Consume our one cleanup attempt before calling a potentially
            // blocking driver. EOF, timeout, and later requests cannot replay it.
            owns = false;
            uncertain = true;
            if (modern) Call("Disconnect"); else Set("Connected", false);
            uncertain = false;
            phase = modern ? Phase.WaitClose : Phase.Closed;
        } else phase = Phase.Closed;
        return phase == Phase.Closed;
    }

    public void Close() {
        AssertSta();
        try {
            // No Dispose call: some vendor implementations disconnect global
            // hardware there, even for a borrowed connection. Release only RCW.
            if (driver != null && owns && !uncertain) DisconnectStep();
        } catch { }
        finally {
            try { if (driver != null && Marshal.IsComObject(driver)) Marshal.FinalReleaseComObject(driver); }
            finally { driver = null; }
        }
    }

    private void AssertSta() {
        if (Thread.CurrentThread.ManagedThreadId != sta || Thread.CurrentThread.GetApartmentState() != ApartmentState.STA)
            throw new InvalidOperationException();
    }
    private void RequireReady() {
        if (phase != Phase.Ready || driver == null) throw new NotConnected();
    }
    private object? Invoke(string name, BindingFlags flags, params object[] args) =>
        driver!.GetType().InvokeMember(name, flags, null, driver, args, CultureInfo.InvariantCulture);
    private object? Get(string name) => Invoke(name, BindingFlags.GetProperty);
    private object? Call(string name, params object[] args) => Invoke(name, BindingFlags.InvokeMethod, args);
    private void Set(string name, object value) => Invoke(name, BindingFlags.SetProperty, value);

    private object Read(string member, JsonElement parameters) {
        if (options.DeviceType == "camera" && CameraProperty(member) is HubCameraProperty cameraProperty)
            return ReadCamera(cameraProperty, parameters);
        var common = new Dictionary<string, string>(StringComparer.Ordinal) {
            ["name"] = "Name", ["description"] = "Description", ["driverinfo"] = "DriverInfo", ["driverversion"] = "DriverVersion"
        };
        if (common.TryGetValue(member, out var metadata)) { Fields(parameters); return Text(Get(metadata)); }
        if (member == "connected") { Fields(parameters); return Boolean(Get("Connected")); }
        if (member == "interfaceversion") { Fields(parameters); return Integer(Get("InterfaceVersion"), 1, short.MaxValue); }
        if (member == "connecting" && modern) { Fields(parameters); return Boolean(Get("Connecting")); }
        if (options.DeviceType == "covercalibrator") {
            foreach (HubCoverCalibratorProperty property in Enum.GetValues(typeof(HubCoverCalibratorProperty))) {
                if (HubCoverCalibratorProtocol.Key(property).ToLowerInvariant() != member) continue;
                Fields(parameters);
                var result = Get(property.ToString());
                object scalar = property switch {
                    HubCoverCalibratorProperty.CoverMoving or HubCoverCalibratorProperty.CalibratorChanging => Boolean(result),
                    HubCoverCalibratorProperty.CoverState when result is global::ASCOM.DeviceInterface.CoverStatus state => (int)state,
                    HubCoverCalibratorProperty.CalibratorState when result is global::ASCOM.DeviceInterface.CalibratorStatus state => (int)state,
                    _ => Int32(result)
                };
                try { HubCoverCalibratorProtocol.Validate(property, JsonSerializer.SerializeToElement(scalar)); }
                catch (HubException) { throw new BadValue(); }
                return scalar;
            }
        }
        if (options.DeviceType == "filterwheel") {
            foreach (HubFilterWheelProperty property in Enum.GetValues(typeof(HubFilterWheelProperty))) {
                if (HubFilterWheelProtocol.Key(property).ToLowerInvariant() != member) continue;
                Fields(parameters);
                var result = Get(property.ToString());
                object value;
                if (property == HubFilterWheelProperty.Position) value = Integer(result,-1,HubFilterWheelProtocol.MaximumSlots-1);
                else {
                    // Reject shape/resource violations before enumerating or
                    // serializing a vendor SAFEARRAY. Never coerce string or
                    // floating values into signed offsets.
                    if (result is not Array array || array.Rank != 1 || array.Length is < 1 or > HubFilterWheelProtocol.MaximumSlots) throw new BadValue();
                    if (property == HubFilterWheelProperty.Names) {
                        value = ReadStrings(array,HubFilterWheelProtocol.MaximumSlots);
                    } else {
                        var offsets = new List<int>(array.Length);
                        foreach (var item in array) offsets.Add(Int32(item));
                        value = offsets.ToArray();
                    }
                }
                try {HubFilterWheelProtocol.Validate(property,JsonSerializer.SerializeToElement(value));}
                catch (HubException) {throw new BadValue();}
                return value;
            }
        }
        if (options.DeviceType == "focuser") {
            foreach (HubFocuserProperty property in Enum.GetValues(typeof(HubFocuserProperty))) {
                if (HubFocuserProtocol.Key(property).ToLowerInvariant() != member) continue;
                Fields(parameters);
                var result = Get(property.ToString());
                object scalar = property switch {
                    HubFocuserProperty.Absolute or HubFocuserProperty.TempCompAvailable or HubFocuserProperty.IsMoving or HubFocuserProperty.TempComp => Boolean(result),
                    HubFocuserProperty.MaxStep or HubFocuserProperty.MaxIncrement or HubFocuserProperty.Position => Int32(result),
                    _ => Number(result)
                };
                try { HubFocuserProtocol.Validate(property, JsonSerializer.SerializeToElement(scalar)); }
                catch (HubException) { throw new BadValue(); }
                return scalar;
            }
        }
        if (options.DeviceType == "rotator") {
            foreach (HubRotatorProperty property in Enum.GetValues(typeof(HubRotatorProperty))) {
                if (HubRotatorProtocol.Key(property).ToLowerInvariant() != member) continue;
                Fields(parameters);
                var result = Get(property.ToString());
                object scalar = property is HubRotatorProperty.CanReverse or HubRotatorProperty.IsMoving or HubRotatorProperty.Reverse
                    ? Boolean(result) : Number(result);
                try { HubRotatorProtocol.Validate(property, JsonSerializer.SerializeToElement(scalar)); }
                catch (HubException) { throw new BadValue(); }
                return scalar;
            }
        }
        if (options.DeviceType == "safetymonitor" && member == "issafe") { Fields(parameters); return Boolean(Get("IsSafe")); }
        if (options.DeviceType == "switch") {
            if (member == "maxswitch") { Fields(parameters); return Integer(Get("MaxSwitch"), 0, short.MaxValue); }
            var methods = new Dictionary<string, string>(StringComparer.Ordinal) {
                ["getswitch"] = "GetSwitch", ["getswitchvalue"] = "GetSwitchValue", ["getswitchname"] = "GetSwitchName",
                ["getswitchdescription"] = "GetSwitchDescription", ["canwrite"] = "CanWrite", ["minswitchvalue"] = "MinSwitchValue",
                ["maxswitchvalue"] = "MaxSwitchValue", ["switchstep"] = "SwitchStep"
            };
            if (methods.TryGetValue(member, out var method)) {
                Fields(parameters, "Id");
                var result = Call(method, Id(parameters));
                if (member is "getswitch" or "canwrite") return Boolean(result);
                if (member is "getswitchname" or "getswitchdescription") return Text(result);
                return Number(result);
            }
        }
        if (options.DeviceType == "observingconditions") {
            var properties = new Dictionary<string, string>(StringComparer.Ordinal) {
                ["averageperiod"] = "AveragePeriod", ["cloudcover"] = "CloudCover", ["dewpoint"] = "DewPoint",
                ["humidity"] = "Humidity", ["pressure"] = "Pressure", ["rainrate"] = "RainRate", ["skybrightness"] = "SkyBrightness",
                ["skyquality"] = "SkyQuality", ["skytemperature"] = "SkyTemperature", ["starfwhm"] = "StarFWHM",
                ["temperature"] = "Temperature", ["winddirection"] = "WindDirection", ["windgust"] = "WindGust", ["windspeed"] = "WindSpeed"
            };
            if (properties.TryGetValue(member, out var property)) { Fields(parameters); return Number(Get(property)); }
            if (member is "timesincelastupdate" or "sensordescription") {
                Fields(parameters, "SensorName");
                var sensor = Parameter(parameters, "SensorName");
                if (sensor.ValueKind != JsonValueKind.String) throw new InvalidInput();
                var name = sensor.GetString();
                if (name == null || name.Length != 0 && !properties.ContainsKey(name)) throw new InvalidInput();
                // ASCOM uses canonical property spelling; upstream case quirks
                // must not be exposed to configuration or duplicated frontends.
                var result = Call(member == "timesincelastupdate" ? "TimeSinceLastUpdate" : "SensorDescription",
                    name.Length == 0 ? "" : properties[name]);
                return member == "sensordescription" ? Text(result) : Number(result);
            }
        }
        throw new Unsupported();
    }

    private object? Write(string member, JsonElement parameters) {
        if (options.DeviceType == "camera") return WriteCamera(member, parameters);
        if (options.DeviceType == "covercalibrator") {
            if (member == "calibratoron") {
                Fields(parameters, "Brightness");
                var item = Parameter(parameters, "Brightness");
                if (item.ValueKind != JsonValueKind.Number || !item.TryGetInt32(out var brightness) || brightness < 0) throw new InvalidInput();
                Call("CalibratorOn", brightness); return null;
            }
            var method = member switch { "opencover" => "OpenCover", "closecover" => "CloseCover",
                "haltcover" => "HaltCover", "calibratoroff" => "CalibratorOff", _ => null };
            if (method is not null) { Fields(parameters); Call(method); return null; }
        }
        if (options.DeviceType == "filterwheel" && member == "position") {
            Fields(parameters,"Position");
            var item = Parameter(parameters,"Position");
            if (item.ValueKind != JsonValueKind.Number || !item.TryGetInt16(out var position)
                || position < 0 || position >= HubFilterWheelProtocol.MaximumSlots) throw new InvalidInput();
            Set("Position",position); return null;
        }
        if (options.DeviceType == "rotator") {
            var method = member switch { "move" => "Move", "moveabsolute" => "MoveAbsolute",
                "movemechanical" => "MoveMechanical", "sync" => "Sync", _ => null };
            if (method is not null) {
                Fields(parameters, "Position");
                var number = InputNumber(Parameter(parameters, "Position"));
                if (Math.Abs(number) > float.MaxValue || member != "move" && (number < 0 || number >= 360)) throw new InvalidInput();
                var position = (float)number;
                try { HubRotatorProtocol.ValidateCommand(position, member != "move"); }
                catch (ArgumentOutOfRangeException) { throw new InvalidInput(); }
                Call(method, position); return null;
            }
            if (member == "halt") { Fields(parameters); Call("Halt"); return null; }
            if (member == "reverse") {
                Fields(parameters, "Reverse");
                var item = Parameter(parameters, "Reverse");
                if (item.ValueKind is not (JsonValueKind.True or JsonValueKind.False)) throw new InvalidInput();
                Set("Reverse", item.GetBoolean()); return null;
            }
        }
        if (options.DeviceType == "focuser") {
            if (member == "move") {
                Fields(parameters, "Position");
                var item = Parameter(parameters, "Position");
                if (item.ValueKind != JsonValueKind.Number || !item.TryGetInt32(out var position)) throw new InvalidInput();
                Call("Move", position); return null;
            }
            if (member == "halt") { Fields(parameters); Call("Halt"); return null; }
            if (member == "tempcomp") {
                Fields(parameters, "TempComp");
                var item = Parameter(parameters, "TempComp");
                if (item.ValueKind is not (JsonValueKind.True or JsonValueKind.False)) throw new InvalidInput();
                Set("TempComp", item.GetBoolean()); return null;
            }
        }
        if (options.DeviceType == "switch" && member is "setswitch" or "setswitchvalue") {
            var key = member == "setswitch" ? "State" : "Value";
            Fields(parameters, "Id", key);
            var id = Id(parameters);
            var item = Parameter(parameters, key);
            if (member == "setswitch" && item.ValueKind is not (JsonValueKind.True or JsonValueKind.False)) throw new InvalidInput();
            object value = member == "setswitch" ? item.GetBoolean() : InputNumber(item);
            Call(member == "setswitch" ? "SetSwitch" : "SetSwitchValue", id, value);
            return null;
        }
        if (options.DeviceType == "observingconditions" && member == "averageperiod") {
            Fields(parameters, "AveragePeriod");
            var value = InputNumber(Parameter(parameters, "AveragePeriod"));
            if (value < 0) throw new InvalidInput();
            Set("AveragePeriod", value);
            return null;
        }
        // Connection mutation, arbitrary Action/Command, and SetupDialog are
        // deliberately absent. They cannot bypass source leases or run in polls.
        throw new Unsupported();
    }

    private static void Fields(JsonElement parameters, params string[] names) {
        var actual = parameters.ValueKind == JsonValueKind.Object ? parameters.EnumerateObject().Select(p => p.Name).ToArray() : Array.Empty<string>();
        if (actual.Length != names.Length || actual.Except(names, StringComparer.Ordinal).Any()) throw new InvalidInput();
    }
    private static JsonElement Parameter(JsonElement parameters, string name) {
        if (!parameters.TryGetProperty(name, out var value)) throw new InvalidInput();
        return value;
    }
    private static short Id(JsonElement parameters) {
        var value = Parameter(parameters, "Id");
        if (value.ValueKind != JsonValueKind.Number || !value.TryGetInt16(out var number) || number < 0) throw new InvalidInput();
        return number;
    }
    private static double InputNumber(JsonElement value) {
        if (value.ValueKind != JsonValueKind.Number || !value.TryGetDouble(out var number)
            || double.IsNaN(number) || double.IsInfinity(number)) throw new InvalidInput();
        return number;
    }
    private static bool Boolean(object? value) => value is bool boolean ? boolean : throw new BadValue();
    private static int Int32(object? value) => value is int integer ? integer : value is short small ? small : throw new BadValue();
    private static short Integer(object? value, int min, int max) {
        if (value is not (short or int)) throw new BadValue();
        var number = Convert.ToInt32(value, CultureInfo.InvariantCulture);
        return number >= min && number <= max ? (short)number : throw new BadValue();
    }
    private static double Number(object? value) {
        if (value is not (double or float or short or int or long)) throw new BadValue();
        var number = Convert.ToDouble(value, CultureInfo.InvariantCulture);
        return double.IsNaN(number) || double.IsInfinity(number) ? throw new BadValue() : number;
    }
    private static string Text(object? value) => value is string text && text.Length <= 65536 ? text : throw new BadValue();
    private static Exception Unwrap(Exception error) {
        while (error is TargetInvocationException && error.InnerException != null) error = error.InnerException;
        return error;
    }
    private static string Classify(Exception error, bool mutation) {
        var code = error.HResult;
        if (error is Unsupported or MissingMethodException || code == global::ASCOM.ErrorCodes.NotImplemented
            || code == global::ASCOM.ErrorCodes.ActionNotImplementedException || code == unchecked((int)0x80020003)) return "unsupported";
        // Only our pre-dispatch input checks or the defined ASCOM rejection
        // prove invalid input. Arbitrary vendor ArgumentException is ambiguous.
        if (error is InvalidInput || code == global::ASCOM.ErrorCodes.InvalidValue) return "invalidValue";
        if (error is NotConnected || code == global::ASCOM.ErrorCodes.NotConnected) return "disconnected";
        if (error is BadValue || code == global::ASCOM.ErrorCodes.ValueNotSet || code == global::ASCOM.ErrorCodes.NotInCacheException) return "unavailable";
        if (code == unchecked((int)0x80040154) || code == unchecked((int)0x800401F3) || error is ConnectionFault) return "permanent";
        // A driver-specific failure after a setter/method may follow a real
        // hardware change. Preserve HRESULT and prohibit automatic replay.
        return mutation ? "uncertain" : "transient";
    }
    private sealed class Unsupported : Exception { }
    private sealed class InvalidInput : Exception { }
    private sealed class BadValue : Exception { }
    private sealed class NotConnected : Exception { }
    private sealed class ConnectionFault : Exception { }
    private sealed class UncertainCommand : Exception { }
}
