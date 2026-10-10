using System.Runtime.InteropServices;
using System.Text.Json;
using ASCOM.DeviceInterface;
using Regain.Hub;
using Regain.Hub.ASCOM;

internal static partial class NativeOutputs
{
    internal static async Task Run(string executable, string path, Guid instance, JsonElement config, HubClient inspector, CancellationToken token)
    {
        HubSelection Binding(int index, string type) => new() { ConfigPath = path, InstanceId = instance,
            OutputId = config.GetProperty("outputs")[index].GetProperty("id").GetGuid(), DeviceType = type,
            Label = "ASCOM output fixture", Simulated = true };
        var binding = Binding(0, "switch");
        var vector = new HubSelection { InstanceId = Guid.Parse("10000000-0000-0000-0000-000000000001"),
            OutputId = Guid.Parse("20000000-0000-0000-0000-000000000002"), DeviceType = "switch" };
        Require(OutputIdentity.ClassId(vector).ToString() == "69a5917f-8d71-5a9d-b3e7-8d5a53f88e0b", "Cross-language output identity vector mismatch");
        binding.Simulated = false; // Saved metadata can predate a config update.
        using var first = new SwitchOutput(binding, executable);
        using var sibling = new SwitchOutput(binding, executable);
        Query<ISwitchV3>(first); Query<ISwitchV2>(first);
        Require(first.InterfaceVersion == 3 && first.SupportedActions.Count == 0, "Switch metadata");
        Expect<global::ASCOM.NotConnectedException>(() => first.GetSwitchValue(1));
        Require(!first.Connected && !first.Connecting, "Metadata activated equipment");
        first.Connect();
        await Until(() => !first.Connecting, token);
        Require(first.Connected, "Modern switch connect failed");
        Require(first.Name.Contains("SIMULATION"), "Live simulation marking did not replace stale saved metadata");
        sibling.Connected = true;
        Require(sibling.MaxSwitch == 3 && sibling.CanWrite(1) && !sibling.CanWrite(2), "Switch capability mapping");
        await Until(() => { try { return first.GetSwitchValue(1) == 0; } catch (global::ASCOM.ValueNotSetException) { return false; } }, token);
        Require(first.MinSwitchValue(1) == 0 && first.MaxSwitchValue(1) == 100 && first.SwitchStep(1) == 1, "Switch range mapping");
        Expect<global::ASCOM.InvalidValueException>(() => first.SetSwitchValue(-1, 2));
        Expect<global::ASCOM.InvalidValueException>(() => first.SetSwitchValue(1, double.NaN));
        Expect<global::ASCOM.InvalidValueException>(() => first.SetSwitchValue(1, 101));
        first.SetSwitchValue(1, 46);
        await Until(() => sibling.GetSwitchValue(1) == 46, token);
        Require(!first.CanAsync(1), "Unsupported async switch was advertised");
        Expect<global::ASCOM.MethodNotImplementedException>(() => first.SetAsyncValue(1, 47));
        Expect<global::ASCOM.MethodNotImplementedException>(() => first.StateChangeComplete(1));
        first.CancelAsync(1);
        Require(sibling.GetSwitchValue(1) == 46, "Rejected operation changed the channel");
        Require(first.DeviceState.Count >= 2, "Switch DeviceState missing");
        AssertNoTimestamp(first.DeviceState);
        first.Disconnect(); await Until(() => !first.Connecting, token);
        Require(!first.Connected && sibling.Connected && sibling.GetSwitchValue(1) == 46, "One ASCOM client revoked another");
        first.Connected = true;
        first.Dispose();
        Expect<global::ASCOM.NotConnectedException>(() => first.Connect());
        Require(sibling.Connected && sibling.GetSwitchValue(1) == 46, "Dispose revoked another client");
        sibling.Connected = false;

        using var safety = new SafetyOutput(Binding(1, "safetymonitor"), executable);
        Query<ISafetyMonitorV3>(safety); Query<ISafetyMonitor>(safety);
        safety.Connected = true;
        Require(safety.InterfaceVersion == 3 && !safety.IsSafe, "Safety initial fail-closed state");
        var state = safety.DeviceState;
        Require(state.Count == 1 && state[0].Name == "IsSafe" && state[0].Value is false, "Typed safety DeviceState");
        safety.Connected = false;

        using var weather = new WeatherOutput(Binding(2, "observingconditions"), executable);
        Query<IObservingConditionsV2>(weather); Query<IObservingConditions>(weather);
        weather.Connect(); await Until(() => !weather.Connecting, token);
        await Until(() => { try { return weather.Temperature == 12 && weather.Pressure == 1013; } catch (global::ASCOM.ValueNotSetException) { return false; } }, token);
        Require(weather.InterfaceVersion == 2 && weather.AveragePeriod == 0, "Weather metadata");
        Require(weather.SensorDescription("TEMPERATURE").Contains("temperature") && weather.TimeSinceLastUpdate("temperature") >= 0, "Weather method mapping");
        Expect<global::ASCOM.PropertyNotImplementedException>(() => _ = weather.Humidity);
        Expect<global::ASCOM.MethodNotImplementedException>(() => weather.SensorDescription("humidity"));
        Expect<global::ASCOM.InvalidValueException>(() => weather.TimeSinceLastUpdate("unknown"));
        Expect<global::ASCOM.InvalidValueException>(() => weather.AveragePeriod = double.PositiveInfinity);
        weather.AveragePeriod = 0.01; Require(weather.AveragePeriod == 0.01, "Weather averaging update");
        weather.AveragePeriod = 0;
        AssertNoTimestamp(weather.DeviceState);
        await inspector.RequestAsync(JsonSerializer.SerializeToElement(new { op = "updateSimulation",
            source = config.GetProperty("sources")[2].GetProperty("id").GetGuid(), update = new { weather = new { temperature = (double?)null, pressure = 1001.0 } } }), token);
        await Until(() => {
            try { _ = weather.Temperature; return false; }
            catch (global::ASCOM.ValueNotSetException) { return weather.Pressure == 1001; }
        }, token);
        Require(weather.Connected, "Missing metric retired weather connection");
        weather.Disconnect(); await Until(() => !weather.Connecting, token);
        Require(!weather.Connected, "Weather disconnect failed");

        var missing = binding.Copy(); missing.OutputId = Guid.NewGuid();
        using var failed = new SwitchOutput(missing, executable);
        failed.Connect();
        await Until(() => { try { return !failed.Connecting && false; } catch (global::ASCOM.DriverException) { return true; } }, token);
        Expect<global::ASCOM.DriverException>(() => _ = failed.Connecting);
        Expect<global::ASCOM.DriverException>(() => _ = failed.Connecting);
        failed.Disconnect(); await Until(() => !failed.Connecting, token);
        Require(!failed.Connected, "Failed connection could not reconcile explicitly");
        Console.WriteLine($"ASCOM adapters {IntPtr.Size * 8}-bit: current/legacy COM QI, typed values/errors, modern/legacy connections, independent leases, weather availability and retained failure passed");
    }
    private static void Query<T>(object driver)
    {
        var unknown = Marshal.GetIUnknownForObject(driver);
        IntPtr pointer = IntPtr.Zero;
        try {
            var iid = typeof(T).GUID;
            Marshal.ThrowExceptionForHR(Marshal.QueryInterface(unknown, ref iid, out pointer));
        } finally { if (pointer != IntPtr.Zero) Marshal.Release(pointer); Marshal.Release(unknown); }
    }
    private static void AssertNoTimestamp(IStateValueCollection states)
    {
        for (var index = 0; index < states.Count; index++) Require(states[index].Name != "TimeStamp", "Query clock masqueraded as measurement time");
    }
    private static void Require(bool condition, string message) { if (!condition) throw new InvalidOperationException(message); }
    private static void Expect<T>(Action action) where T : Exception
    {
        try { action(); } catch (T) { return; }
        throw new InvalidOperationException("Expected " + typeof(T).Name);
    }
    private static async Task Until(Func<bool> condition, CancellationToken token)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(token);
        deadline.CancelAfter(TimeSpan.FromSeconds(12));
        while (true) {
            try { if (condition()) return; }
            catch (global::ASCOM.ValueNotSetException) { } // Wait only for a real sampled value, never invent one.
            await Task.Delay(25, deadline.Token);
        }
    }
}
