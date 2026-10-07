using System.ComponentModel.Composition;
using System.Diagnostics;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using NINA.Equipment.Interfaces;
using Regain.Hub;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    [Fact]
    public async Task SafetyFixtureContinuesAfterAnAbortedPoll()
    {
        await using var server = new SafetyServer();
        var endpoint = new Uri(server.Url);
        using (var aborted = new TcpClient()) {
            await aborted.ConnectAsync(endpoint.Host, endpoint.Port);
            await aborted.GetStream().WriteAsync(Encoding.ASCII.GetBytes("GET /issafe HTTP/1.1\r\nHost: fixture\r\n"));
            aborted.Client.LingerState = new LingerOption(true, 0);
        }
        using var http = new HttpClient { Timeout = TimeSpan.FromSeconds(5) };
        using var response = await http.GetAsync(server.Url + "/issafe");
        response.EnsureSuccessStatusCode();
        using var body = JsonDocument.Parse(await response.Content.ReadAsStringAsync());
        Assert.True(body.RootElement.GetProperty("Value").GetBoolean());
    }
    [Fact]
    public async Task SafetyFixtureReportsRetryAfterOnFailedPolls()
    {
        await using var server = new SafetyServer { Failing = true };
        using var http = new HttpClient { Timeout = TimeSpan.FromSeconds(5) };
        using var failed = await http.GetAsync(server.Url + "/issafe");
        Assert.Equal(HttpStatusCode.ServiceUnavailable, failed.StatusCode);
        Assert.Equal(TimeSpan.FromSeconds(2), failed.Headers.RetryAfter!.Delta);
        using var metadata = await http.GetAsync(server.Url + "/interfaceversion");
        Assert.Equal(HttpStatusCode.OK, metadata.StatusCode);
        Assert.Null(metadata.Headers.RetryAfter);
    }
    private static HubSelection Binding(string directory, string type = "switch") => new() {
        ConfigPath = Path.Combine(directory, "configuration.json"), InstanceId = Guid.NewGuid(), OutputId = Guid.NewGuid(),
        DeviceType = type, Label = "Saved simulation", Simulated = true
    };
    [Fact]
    public async Task NativeAttachmentHasNoEquipmentLeaseAndKeepsClientsIndependent()
    {
        await using var host = await Host.Open();
        var binding = host.Selection(0, "switch");
        using var attached = new HubNativeSession(host.Executable, host.Workers);
        using var sibling = new HubNativeSession(host.Executable, host.Workers);
        await attached.AttachAsync(binding, CancellationToken.None);
        Assert.True(attached.IsAttached); Assert.False(attached.Connected);
        Assert.Equal(0, (await host.Status(0)).GetProperty("leaseCount").GetInt32());
        var query = JsonSerializer.SerializeToElement(new { op = "get", output = binding.OutputId, property = new { member = "connected" } });
        Assert.False((await attached.RequestAsync(attached.Epoch, query)).GetBoolean());
        await Assert.ThrowsAsync<InvalidOperationException>(() => attached.ConnectAsync(binding, CancellationToken.None));
        var change = JsonSerializer.SerializeToElement(new { op = "changeConnection", output = binding.OutputId, connected = true, asynchronous = false });
        await attached.RequestAsync(attached.Epoch, change, TimeSpan.FromSeconds(35));
        await sibling.ConnectAsync(binding, CancellationToken.None);
        Assert.Equal(2, (await host.Status(0)).GetProperty("leaseCount").GetInt32());
        var retired = attached.Epoch;
        attached.Disconnect();
        await Eventually(async () => (await host.Status(0)).GetProperty("leaseCount").GetInt32() == 1);
        Assert.True(sibling.Connected);
        await Assert.ThrowsAsync<InvalidOperationException>(() => attached.RequestAsync(retired, query));
        await attached.AttachAsync(binding, CancellationToken.None);
        Assert.NotEqual(retired, attached.Epoch);
        Assert.False((await attached.RequestAsync(attached.Epoch, query)).GetBoolean());
        Assert.True((await sibling.RequestAsync(sibling.Epoch, query)).GetBoolean());
    }
    [Fact]
    public void SelectionsPersistOnlyIdentitiesAndRejectCompetingEditors()
    {
        var directory = Path.Combine(Path.GetTempPath(), "Regain bindings " + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try {
            var store = new HubSelectionStore(Path.Combine(directory, "bindings.json"));
            Assert.Equal(Guid.Empty, store.Load().Revision);
            var a = Binding(directory); a.Label = new string('\u03bb', 200);
            var first = store.Save(a, Guid.Empty);
            Assert.DoesNotContain("\"id\"", File.ReadAllText(store.Path));
            Assert.DoesNotContain("source", File.ReadAllText(store.Path));
            Assert.Equal(a.Id, Assert.Single(store.Load().Bindings).Id);
            Assert.Throws<InvalidOperationException>(() => store.Save(a, Guid.Empty));
            var b = Binding(directory, "safetymonitor");
            var second = store.Save(b, first.Revision);
            Assert.Equal(2, second.Bindings.Length);
            a.Label = "Renamed";
            var third = store.Save(a, second.Revision);
            Assert.Equal(2, third.Bindings.Length);
            Assert.Equal(a.Id, third.Bindings.Single(s => s.Label == "Renamed").Id);
            Assert.Throws<InvalidOperationException>(() => store.Save(b, second.Revision));
        } finally { Directory.Delete(directory, true); }
    }
    [Theory]
    [InlineData("null")]
    [InlineData("{\"configPath\":null,\"label\":null}")]
    [InlineData("{\"configPath\":\"C:relative.json\",\"instanceId\":\"10000000-0000-0000-0000-000000000001\",\"outputId\":\"10000000-0000-0000-0000-000000000002\",\"deviceType\":\"switch\",\"label\":\"Wrong path\"}")]
    public void InvalidBindingsAreNotEnumerated(string binding)
    {
        var path = Path.Combine(Path.GetTempPath(), "Regain invalid " + Guid.NewGuid().ToString("N") + ".json");
        try {
            File.WriteAllText(path, "{\"schemaVersion\":1,\"revision\":\"10000000-0000-0000-0000-000000000003\",\"bindings\":[" + binding + "]}");
            var store = new HubSelectionStore(path);
            Assert.Throws<InvalidOperationException>(() => store.Load());
            var choices = HubEquipment.Choices<ISwitchHub>("switch", s => new HubSwitchDevice(s), store);
            Assert.EndsWith("Configure.switch", Assert.Single(choices).Id);
            foreach (var choice in choices) ((IDisposable)choice).Dispose();
        } finally { File.Delete(path); }
    }
    [Fact]
    public void ProviderEnumerationNeedsNoHostAndPreservesOutputIdentities()
    {
        var directory = Path.Combine(Path.GetTempPath(), "Regain enumerate " + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try {
            var store = new HubSelectionStore(Path.Combine(directory, "bindings.json"));
            var a = Binding(directory); var b = Binding(directory); var safety = Binding(directory, "safetymonitor");
            var saved = store.Save(a, Guid.Empty); saved = store.Save(b, saved.Revision); store.Save(safety, saved.Revision);
            // None of these configurations or host executables exists. Reading
            // the chooser must neither launch a host nor inspect equipment.
            var choices = HubEquipment.Choices<ISwitchHub>("switch", s => new HubSwitchDevice(s, "missing.exe"), store);
            Assert.Equal(new[] { a.Id, b.Id, "PulsarFab.Regain.Hub.Configure.switch" }, choices.Select(c => c.Id));
            Assert.All(choices, c => Assert.False(c.Connected));
            Assert.Contains("SIMULATION", choices[0].Name);
            foreach (var choice in choices) ((IDisposable)choice).Dispose();
            foreach (var provider in new[] { typeof(HubSwitchProvider), typeof(HubSafetyProvider), typeof(HubWeatherProvider) })
                Assert.Single(provider.GetCustomAttributes(typeof(ExportAttribute), false));
        } finally { Directory.Delete(directory, true); }
    }
    [Fact]
    public async Task NativeSwitchesShareTheRealHostAndFenceRetiredChannelObjects()
    {
        await using var host = await Host.Open();
        using var first = host.Switch(); using var second = host.Switch();
        await first.Connect(CancellationToken.None); await second.Connect(CancellationToken.None);
        Assert.Equal(new short[] { 0, 1, 2 }, first.Switches.Select(c => c.Id));
        Assert.IsNotAssignableFrom<IWritableSwitch>(first.Switches.Single(c => c.Id == 2));
        var level = Assert.IsAssignableFrom<IWritableSwitch>(first.Switches.Single(c => c.Id == 1));
        await Eventually(() => level.Poll());
        Assert.Equal(0, level.Value);
        Assert.Throws<ArgumentOutOfRangeException>(() => level.TargetValue = double.NaN);
        Assert.Throws<ArgumentOutOfRangeException>(() => level.TargetValue = 101);
        level.TargetValue = 37.5; Assert.Equal(38, level.TargetValue);
        level.SetValue();
        var shared = second.Switches.Single(c => c.Id == 1);
        await Eventually(() => shared.Poll() && shared.Value == 38);
        Assert.Equal(2, (await host.Status(0)).GetProperty("leaseCount").GetInt32());
        first.Disconnect();
        await Eventually(async () => (await host.Status(0)).GetProperty("leaseCount").GetInt32() == 1);
        Assert.False(level.Poll()); Assert.Throws<InvalidOperationException>(() => level.Value);
        Assert.True(second.Connected); Assert.True(shared.Poll()); Assert.Equal(38, shared.Value);
        await first.Connect(CancellationToken.None);
        level.TargetValue = 60;
        Assert.Throws<InvalidOperationException>(level.SetValue);
        Assert.True(shared.Poll()); Assert.Equal(38, shared.Value);
        Assert.NotSame(level, first.Switches.Single(c => c.Id == 1));
        first.Disconnect(); second.Disconnect();
        Assert.Equal("ready", (await host.Command(new { op = "hostStatus" })).GetProperty("phase").GetString());
    }
    [Fact]
    public async Task UncertainSwitchWritesNeverBecomeSuccessfulNanCompletion()
    {
        await using var host = await Host.Open();
        using var device = host.Switch(); await device.Connect(CancellationToken.None);
        var level = Assert.IsAssignableFrom<IWritableSwitch>(device.Switches.Single(c => c.Id == 1));
        await Eventually(() => level.Poll());
        await host.Update(0, new { fault = "uncertainWrite" });
        level.TargetValue = 41;
        var error = Assert.Throws<HubException>(level.SetValue);
        Assert.Equal("uncertain", error.Remote!.Code);
        Assert.Throws<InvalidOperationException>(() => level.Value);
        var state = await host.Update(0, new { fault = "none" });
        Assert.Equal(41, state.GetProperty("switchValues").GetProperty("1").GetDouble());
        level.TargetValue = 70;
        Assert.Equal("uncertain", Assert.Throws<HubException>(level.SetValue).Remote!.Code);
        state = await host.Update(0, new { });
        Assert.Equal(41, state.GetProperty("switchValues").GetProperty("1").GetDouble());
        device.Disconnect();
        Assert.False(level.Poll()); Assert.Throws<InvalidOperationException>(() => level.Value);
    }
    [Fact]
    public async Task NativeSafetyUsesSharedPolicyAndRevokesOnTransportLoss()
    {
        await using var host = await Host.Open();
        using var device = host.Safety(); Assert.False(device.IsSafe);
        await device.Connect(CancellationToken.None); Assert.False(device.IsSafe);
        await host.Update(1, new { safe = true });
        await Eventually(() => device.IsSafe);
        await host.Update(1, new { fault = "timeout" });
        // A source transport reset revokes permission independently of frontend
        // connectivity. The output remains available to report unsafe.
        await Eventually(() => !device.IsSafe);
        Assert.True(device.Connected);
        await host.Update(1, new { fault = "none", safe = true }); await Eventually(() => device.IsSafe);
        await host.Stop();
        Assert.False(device.IsSafe); Assert.False(device.Connected);
    }
    [Fact]
    public async Task NativeSafetyExpiresDuringHttpBackoffWhileOtherReadingsRemainAvailable()
    {
        await using var upstream = new SafetyServer();
        await using var host = await Host.Open(config => config["sources"]![1]!["backend"] = JsonSerializer.SerializeToNode(new {
            kind = "alpaca", baseUrl = upstream.Url, deviceType = "safetymonitor", deviceNumber = 0, connectionPolicy = "externallyManaged"
        }));
        using var safety = host.Safety(); using var weather = host.Weather();
        await safety.Connect(CancellationToken.None); await weather.Connect(CancellationToken.None);
        await Eventually(() => safety.IsSafe);
        await host.Command(new { op = "changeConnection", output = host.Selection(1, "safetymonitor").OutputId, connected = true, asynchronous = false });
        var before = await host.Status(1);
        upstream.Failing = true;
        var clock = Stopwatch.StartNew();
        // Establish an acknowledged HTTP backoff rather than assuming the
        // frontend becoming unsafe proves expiry without a transport reset.
        // The fixture's Retry-After exceeds the maximum safe age.
        await Eventually(async () => {
            var state = await host.Status(1);
            var error = state.GetProperty("error");
            return error.ValueKind == JsonValueKind.Object &&
                error.GetProperty("upstreamCode").ValueKind == JsonValueKind.Number &&
                error.GetProperty("upstreamCode").GetInt32() == 503;
        });
        var backoff = await host.Status(1);
        Assert.Equal(before.GetProperty("generation").GetString(), backoff.GetProperty("generation").GetString());
        Assert.True(safety.IsSafe);
        var polling = backoff.GetProperty("polling");
        Assert.Equal("waiting", polling.GetProperty("phase").GetString());
        Assert.Equal("retry", polling.GetProperty("reason").GetString());
        Assert.InRange(polling.GetProperty("nextPollAfterSeconds").GetDouble(), 1.5, 2.0);
        await Eventually(() => !safety.IsSafe);
        Assert.True(safety.Connected); Assert.Equal(12, weather.Temperature);
        var after = await host.Status(1);
        Assert.Equal(before.GetProperty("generation").GetString(), after.GetProperty("generation").GetString());
        var policy = await host.Command(new { op = "get", output = host.Selection(1, "safetymonitor").OutputId, property = new { member = "safetyStatus" } });
        var endpoint = Assert.Single(policy.GetProperty("endpoints").EnumerateObject()).Value;
        Assert.Equal("stale", endpoint.GetProperty("phase").GetString());
        Assert.True(endpoint.GetProperty("failedCycles").GetInt32() < 1000);
        Assert.InRange(clock.Elapsed.TotalSeconds, 0.7, 4.0);
        upstream.Failing = false; await Eventually(() => safety.IsSafe);
    }
    [Fact]
    public async Task WeatherFailuresRemainPerMetricAndStaleReadingsBecomeNan()
    {
        await using var host = await Host.Open();
        using var device = host.Weather(); Assert.True(double.IsNaN(device.Temperature));
        await device.Connect(CancellationToken.None);
        await Eventually(() => device.Temperature == 12 && device.Pressure == 1013);
        Assert.Equal(0, device.AveragePeriod); Assert.True(double.IsNaN(device.Humidity));
        Assert.True(device.Connected);
        await host.Update(2, new { weather = new { temperature = (double?)null, pressure = 1001.0 } });
        await Eventually(() => double.IsNaN(device.Temperature) && device.Pressure == 1001);
        Assert.True(device.Connected);
        await host.Update(2, new { weather = new { temperature = 20.0 }, sampleAgeSeconds = 10.0 });
        await Eventually(() => double.IsNaN(device.Pressure));
        Assert.True(double.IsNaN(device.Temperature));
        await host.Update(2, new { sampleAgeSeconds = 0.0 });
        await Eventually(() => device.Temperature == 20 && device.Pressure == 1001);
        device.Disconnect(); Assert.True(double.IsNaN(device.Pressure));
    }
    [Fact]
    public async Task CancelledOrDisconnectedConnectCannotReviveTheDevice()
    {
        await using var host = await Host.Open();
        using var device = host.Switch();
        using var cancelled = new CancellationTokenSource(); cancelled.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => device.Connect(cancelled.Token));
        Assert.False(device.Connected);
        for (int i = 0; i < 5; i++) {
            var connecting = device.Connect(CancellationToken.None); device.Disconnect();
            await Assert.ThrowsAnyAsync<OperationCanceledException>(() => connecting);
            Assert.False(device.Connected);
        }
        await Eventually(async () => (await host.Status(0)).GetProperty("leaseCount").GetInt32() == 0);
        await device.Connect(CancellationToken.None); Assert.True(device.Connected);
        device.Dispose(); Assert.False(device.Connected);
        await Assert.ThrowsAsync<ObjectDisposedException>(() => device.Connect(CancellationToken.None));
    }
    [Fact]
    public async Task SavedBindingsRejectRetargetingAndMissingOutputs()
    {
        await using var host = await Host.Open();
        var wrongInstance = host.Selection(0, "switch"); wrongInstance.InstanceId = Guid.NewGuid();
        using var session = new HubNativeSession(host.Executable, host.Workers);
        Assert.Equal(HubFailure.Protocol, (await Assert.ThrowsAsync<HubException>(() => session.ConnectAsync(wrongInstance, CancellationToken.None))).Failure);
        var missing = host.Selection(0, "switch"); missing.OutputId = Guid.NewGuid();
        await Assert.ThrowsAsync<InvalidOperationException>(() => session.ConnectAsync(missing, CancellationToken.None));
        Assert.False(session.Connected);
        var valid = host.Selection(0, "switch");
        var connecting = session.ConnectAsync(valid, CancellationToken.None);
        valid.OutputId = Guid.NewGuid(); valid.InstanceId = Guid.NewGuid(); // callers cannot mutate an in-flight binding
        await connecting; Assert.True(session.Connected);
        Assert.Equal(1, (await host.Status(0)).GetProperty("leaseCount").GetInt32());
    }
    [Fact]
    public async Task ReorderingAndRetiringSwitchChannelsDoesNotRetargetSavedNinaObjects()
    {
        await using var host = await Host.Open();
        using var device = host.Switch(); await device.Connect(CancellationToken.None);
        var retired = Assert.IsAssignableFrom<IWritableSwitch>(device.Switches.Single(c => c.Id == 1));
        var deviceId = device.Id; device.Disconnect();
        var config = JsonNode.Parse((await host.Command(new { op = "getConfig" })).GetRawText())!.AsObject();
        var channels = config["outputs"]![0]!["device"]!["channels"]!.AsArray();
        channels.RemoveAt(1);
        var first = channels[0]!.DeepClone(); channels.RemoveAt(0); channels.Add(first);
        config["outputs"]![0]!["label"] = "Renamed output";
        var revision = config["revision"]!.GetValue<string>();
        var applied = await host.Command(new { op = "applyConfig", expectedRevision = revision, candidate = config });
        Assert.True(applied.GetProperty("applied").GetBoolean()); Assert.True(applied.GetProperty("ready").GetBoolean());
        await device.Connect(CancellationToken.None);
        Assert.Equal(deviceId, device.Id); Assert.Contains("Renamed output", device.Name); Assert.Contains("SIMULATION", device.Name);
        Assert.Equal(new short[] { 0, 1, 2 }, device.Switches.Select(c => c.Id));
        var hole = device.Switches.Single(c => c.Id == 1);
        Assert.Contains("Removed", hole.Name); Assert.IsNotAssignableFrom<IWritableSwitch>(hole);
        Assert.False(hole.Poll()); Assert.True(double.IsNaN(hole.Value));
        Assert.False(retired.Poll()); Assert.Throws<InvalidOperationException>(retired.SetValue);
        Assert.Contains("relay", device.Switches.Single(c => c.Id == 0).Name);
    }
    [Fact]
    public async Task UnavailableWritableCapabilitiesDoNotBreakOtherNinaDevices()
    {
        await using var host = await Host.Open();
        await host.Update(0, new { fault = "readError" });
        using var switches = host.Switch(); using var weather = host.Weather();
        await switches.Connect(CancellationToken.None); await weather.Connect(CancellationToken.None);
        Assert.True(switches.Connected); Assert.True(weather.Connected);
        Assert.All(switches.Switches, s => Assert.IsNotAssignableFrom<IWritableSwitch>(s));
        Assert.Contains("capability unavailable", switches.Switches.Single(s => s.Id == 1).Description);
        await Eventually(() => weather.Temperature == 12);
        await host.Update(0, new { fault = "none" });
        switches.Disconnect(); await switches.Connect(CancellationToken.None);
        Assert.IsAssignableFrom<IWritableSwitch>(switches.Switches.Single(s => s.Id == 1));
    }
    [Fact]
    public async Task ActualAlpacaPublisherAndNativeNinaShareSwitchStateAndSeparateLeases()
    {
        await using var host = await Host.Open();
        using var device = host.Switch(); await device.Connect(CancellationToken.None);
        // The local native output already works with only a --hub-host process.
        var reservation = new TcpListener(IPAddress.Loopback, 0); reservation.Start();
        var port = ((IPEndPoint)reservation.LocalEndpoint).Port; reservation.Stop();
        // Keep the HTTP fixture accessory-only and independent of installed
        // user camera profiles, migration and hardware discovery.
        var profiles = Path.Combine(host.DirectoryPath, "empty-camera-profiles.json");
        await File.WriteAllTextAsync(profiles, "[]");
        using var publisher = new Process { StartInfo = new ProcessStartInfo(host.Executable) {
            UseShellExecute = false, CreateNoWindow = true, WindowStyle = ProcessWindowStyle.Hidden,
            RedirectStandardOutput = true, RedirectStandardError = true
        } };
        publisher.StartInfo.ArgumentList.Add("--hub-config"); publisher.StartInfo.ArgumentList.Add(host.ConfigPath);
        publisher.StartInfo.ArgumentList.Add("--profiles"); publisher.StartInfo.ArgumentList.Add(profiles);
        publisher.StartInfo.ArgumentList.Add("--simulate");
        publisher.StartInfo.ArgumentList.Add("--workers"); publisher.StartInfo.ArgumentList.Add(host.Workers);
        publisher.StartInfo.ArgumentList.Add("--no-discovery"); publisher.StartInfo.ArgumentList.Add("--port"); publisher.StartInfo.ArgumentList.Add(port.ToString());
        Assert.True(publisher.Start());
        using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(10));
        var output = publisher.StandardOutput.ReadToEndAsync(timeout.Token); var errors = publisher.StandardError.ReadToEndAsync(timeout.Token);
        Exception? primaryFailure = null;
        try {
            using var http = new HttpClient { BaseAddress = new Uri("http://127.0.0.1:" + port), Timeout = TimeSpan.FromSeconds(3) };
            var transaction = 0;
            async Task<JsonElement> Put(string member, string key, string value)
            {
                using var body = new FormUrlEncodedContent(new Dictionary<string, string> { [key] = value, ["ClientID"] = "87001", ["ClientTransactionID"] = (++transaction).ToString() });
                using var response = await http.PutAsync("/api/v1/switch/0/" + member, body, timeout.Token);
                response.EnsureSuccessStatusCode(); var parsed = JsonDocument.Parse(await response.Content.ReadAsStringAsync(timeout.Token));
                using (parsed) { Assert.Equal(0, parsed.RootElement.GetProperty("ErrorNumber").GetInt32()); return parsed.RootElement.Clone(); }
            }
            async Task<JsonElement> GetReply(string member)
            {
                using var response = await http.GetAsync("/api/v1/switch/0/" + member + "?ClientID=87001&ClientTransactionID=" + (++transaction) + "&Id=1", timeout.Token);
                response.EnsureSuccessStatusCode(); using var parsed = JsonDocument.Parse(await response.Content.ReadAsStringAsync(timeout.Token));
                return parsed.RootElement.Clone();
            }
            async Task<JsonElement> Get(string member)
            {
                var reply = await GetReply(member);
                Assert.Equal(0, reply.GetProperty("ErrorNumber").GetInt32()); return reply.GetProperty("Value").Clone();
            }
            await Eventually(async () => {
                if (publisher.HasExited) throw new InvalidOperationException("Test HTTP publisher exited before readiness");
                try { return (await Get("connected")).ValueKind is JsonValueKind.True or JsonValueKind.False; }
                catch (HttpRequestException) { return false; }
            });
            await Put("connected", "Connected", "true");
            using (var catalog = await http.GetAsync("/management/v1/configureddevices", timeout.Token)) {
                catalog.EnsureSuccessStatusCode();
                using var parsed = JsonDocument.Parse(await catalog.Content.ReadAsStringAsync(timeout.Token));
                Assert.Equal(3, parsed.RootElement.GetProperty("Value").GetArrayLength());
                Assert.DoesNotContain(parsed.RootElement.GetProperty("Value").EnumerateArray(),
                    entry => entry.GetProperty("DeviceType").GetString() == "Camera");
            }
            Assert.Equal(2, (await host.Status(0)).GetProperty("leaseCount").GetInt32());
            var level = Assert.IsAssignableFrom<IWritableSwitch>(device.Switches.Single(s => s.Id == 1));
            level.TargetValue = 29; level.SetValue();
            await Eventually(async () => {
                var reply = await GetReply("getswitchvalue");
                // An ACK deliberately invalidates cached switch samples. Only
                // fresh readback can confirm completion; ValueNotSet is pending,
                // never success. Keep all other errors and the deadline strict.
                if (reply.GetProperty("ErrorNumber").GetInt32() == 0x402) {
                    Assert.Contains("Hub request failed (unavailable)", reply.GetProperty("ErrorMessage").GetString());
                    return false;
                }
                Assert.Equal(0, reply.GetProperty("ErrorNumber").GetInt32());
                return reply.GetProperty("Value").GetDouble() == 29;
            });
            using var write = new FormUrlEncodedContent(new Dictionary<string, string> { ["Id"] = "1", ["Value"] = "77", ["ClientID"] = "87001", ["ClientTransactionID"] = (++transaction).ToString() });
            using var changed = await http.PutAsync("/api/v1/switch/0/setswitchvalue", write, timeout.Token);
            changed.EnsureSuccessStatusCode(); using var reply = JsonDocument.Parse(await changed.Content.ReadAsStringAsync(timeout.Token));
            Assert.Equal(0, reply.RootElement.GetProperty("ErrorNumber").GetInt32());
            await Eventually(() => level.Poll() && level.Value == 77);
            publisher.Kill(); await publisher.WaitForExitAsync();
            await Eventually(async () => (await host.Status(0)).GetProperty("leaseCount").GetInt32() == 1);
            Assert.True(device.Connected); Assert.True(level.Poll()); Assert.Equal(77, level.Value);
            Assert.Equal("ready", (await host.Command(new { op = "hostStatus" })).GetProperty("phase").GetString());
        } catch (Exception error) { primaryFailure = error; throw; }
        finally {
            if (!publisher.HasExited) { publisher.Kill(); await publisher.WaitForExitAsync(); }
            try {
                await Task.WhenAll(output, errors);
                if (primaryFailure is not null) {
                    Console.WriteLine("Private HTTP publisher stdout: " + await output);
                    Console.WriteLine("Private HTTP publisher stderr: " + await errors);
                }
            }
            catch (OperationCanceledException) when (primaryFailure is not null) {
                Console.WriteLine("Publisher cleanup output exceeded its deadline; preserving the original test failure");
            }
        }
    }
    private static Task Eventually(Func<bool> predicate) => Eventually(() => Task.FromResult(predicate()));
    private static async Task Eventually(Func<Task<bool>> predicate)
    {
        using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(5));
        while (!await predicate()) await Task.Delay(20, timeout.Token);
    }
    private sealed class Host : IAsyncDisposable
    {
        internal string DirectoryPath = "", ConfigPath = "", Workers = "", Executable = "";
        private uint? candidate;
        internal JsonObject Config = null!;
        internal HubClient Client = null!;
        internal static async Task<Host> Open(Action<JsonObject>? amend = null)
        {
            var directory = new DirectoryInfo(AppContext.BaseDirectory);
            while (directory is not null && !File.Exists(Path.Combine(directory.FullName, "Cargo.toml"))) directory = directory.Parent;
            var root = directory!.FullName;
            var host = new Host { DirectoryPath = Path.Combine(Path.GetTempPath(), "Regain NINA hub \u03bb " + Guid.NewGuid().ToString("N")),
                Workers = Environment.GetEnvironmentVariable("REGAIN_TEST_WORKERS") ?? Path.Combine(root, "target", "debug") };
            host.Executable = Path.Combine(host.Workers, "regain-alpaca.exe"); Assert.True(File.Exists(host.Executable));
            Directory.CreateDirectory(host.DirectoryPath); host.ConfigPath = Path.Combine(host.DirectoryPath, "configuration.json");
            try {
                host.Config = JsonNode.Parse(File.ReadAllText(Path.Combine(root, "crates", "regain-hub", "examples", "simulated-observatory.json")))!.AsObject();
                host.Config["instanceId"] = Guid.NewGuid().ToString(); host.Config["revision"] = Guid.NewGuid().ToString();
                foreach (var source in host.Config["sources"]!.AsArray()) source!["polling"] = JsonSerializer.SerializeToNode(new {
                    pollSeconds = 0.1, requestTimeoutSeconds = 0.3, attemptsPerCycle = 1, initialBackoffSeconds = 0.05, backoffCapSeconds = 0.05 });
                host.Config["outputs"]![1]!["device"]!["members"]![0]!["policy"] = JsonSerializer.SerializeToNode(new {
                    confirmationSeconds = 0.1, safeReadingsToSafe = 2, returnToSafeHoldSeconds = 0.1, maximumSafeAgeSeconds = 1.2, failedCyclesToUnsafe = 1000 });
                foreach (var measurement in host.Config["outputs"]![2]!["device"]!["measurements"]!.AsObject()) measurement.Value!["maximumAgeSeconds"] = 2.0;
                amend?.Invoke(host.Config);
                await File.WriteAllTextAsync(host.ConfigPath, host.Config.ToJsonString());
                var attachment = await HubAttachment.AttachAsync(host.Executable, host.ConfigPath, host.Workers);
                host.candidate = attachment.StartedProcessId; Assert.NotNull(host.candidate);
                host.Client = await HubClient.ConnectAsync(attachment);
                return host;
            } catch { await host.DisposeAsync(); throw; }
        }
        internal HubSelection Selection(int index, string type) => new() { ConfigPath = ConfigPath,
            InstanceId = Guid.Parse(Config["instanceId"]!.GetValue<string>()), OutputId = Guid.Parse(Config["outputs"]![index]!["id"]!.GetValue<string>()),
            DeviceType = type, Label = "Saved label", Simulated = false };
        internal HubSwitchDevice Switch() => new(Selection(0, "switch"), Executable, Workers);
        internal HubSafetyDevice Safety() => new(Selection(1, "safetymonitor"), Executable, Workers);
        internal HubWeatherDevice Weather() => new(Selection(2, "observingconditions"), Executable, Workers);
        internal Task<JsonElement> Command(object command) => Client.RequestAsync(JsonSerializer.SerializeToElement(command));
        internal Task<JsonElement> Status(int source) => Command(new { op = "sourceStatus", source = Config["sources"]![source]!["id"]!.GetValue<string>() });
        internal Task<JsonElement> Update(int source, object update) => Command(new { op = "updateSimulation", source = Config["sources"]![source]!["id"]!.GetValue<string>(), update });
        internal async Task Stop()
        {
            if (candidate is not uint pid) return;
            try {
                using var process = Process.GetProcessById(checked((int)pid));
                Assert.Equal(Path.GetFullPath(Executable), process.MainModule!.FileName, ignoreCase: true);
                process.Kill(); await process.WaitForExitAsync();
            } catch (ArgumentException) { }
            candidate = null;
        }
        public async ValueTask DisposeAsync() { Client?.Dispose(); await Stop(); Directory.Delete(DirectoryPath, true); }
    }
    private sealed class SafetyServer : IAsyncDisposable
    {
        private readonly TcpListener listener = new(IPAddress.Loopback, 0);
        private readonly CancellationTokenSource stopping = new();
        private readonly Task serving;
        internal volatile bool Failing;
        internal string Url { get; }
        internal SafetyServer()
        {
            listener.Start(); Url = "http://127.0.0.1:" + ((IPEndPoint)listener.LocalEndpoint).Port;
            serving = Serve();
        }
        private async Task Serve()
        {
            try {
                while (!stopping.IsCancellationRequested) {
                    using var socket = await listener.AcceptTcpClientAsync(stopping.Token);
                    try { await Reply(socket); }
                    // The production caller has bounded request deadlines. A
                    // cancelled/timed-out request may close its socket before
                    // the fixture writes. Keep serving subsequent polls.
                    catch (IOException error) when (error.InnerException is SocketException socketError &&
                        socketError.SocketErrorCode is SocketError.ConnectionAborted or SocketError.ConnectionReset or SocketError.Shutdown) { }
                }
            } catch (OperationCanceledException) when (stopping.IsCancellationRequested) { }
            catch (SocketException) when (stopping.IsCancellationRequested) { }
        }
        private async Task Reply(TcpClient socket)
        {
                    using var stream = socket.GetStream();
                    using var reader = new StreamReader(stream, Encoding.ASCII, false, 4096, leaveOpen: true);
                    var first = await reader.ReadLineAsync(stopping.Token);
                    if (first is null) return;
                    string? line;
                    do { line = await reader.ReadLineAsync(stopping.Token); if (line is null) return; } while (line.Length != 0);
                    var target = first.Split(' ')[1];
                    var path = new Uri(Url + target).AbsolutePath;
                    var failed = path.EndsWith("/issafe", StringComparison.Ordinal) && Failing;
                    object value = path.EndsWith("/interfaceversion", StringComparison.Ordinal) ? 3 :
                        path.EndsWith("/connecting", StringComparison.Ordinal) ? false : true;
                    var body = Encoding.UTF8.GetBytes(JsonSerializer.Serialize(new { Value = value, ErrorNumber = 0, ServerTransactionID = 1 }));
                    var header = Encoding.ASCII.GetBytes("HTTP/1.1 " + (failed ? "503 Service Unavailable" : "200 OK") +
                        (failed ? "\r\nRetry-After: 2" : "") +
                        "\r\nContent-Type: application/json\r\nContent-Length: " + body.Length + "\r\nConnection: close\r\n\r\n");
                    await stream.WriteAsync(header, stopping.Token); await stream.WriteAsync(body, stopping.Token);
        }
        public async ValueTask DisposeAsync() { stopping.Cancel(); listener.Stop(); await serving; stopping.Dispose(); }
    }
}
