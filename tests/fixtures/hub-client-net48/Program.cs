using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using Regain.Hub;
using Regain.TestFixtures;

// This executable runs the same public attachment/client API in real net48
// x86/x64 processes. Its caller creates a unique simulated configuration.
internal static class Program
{
    private static async Task<int> Main(string[] args)
    {
        uint? candidate = null;
        try {
            if (args.Length != 3 || IntPtr.Size * 8 != int.Parse(args[2])) throw new InvalidOperationException("Wrong fixture bitness");
            using var focuserServer = new HubFocuserServer();
            var fixtureConfig = JsonNode.Parse(File.ReadAllText(args[1]))!.AsObject();
            focuserServer.AddTo(fixtureConfig, 4, 7);
            var simulatedFocuser = HubFocuserSimulation.AddTo(fixtureConfig,8);
            using var rotatorServer = new HubRotatorServer();
            rotatorServer.AddTo(fixtureConfig, 4, 7);
            var simulatedRotator = HubRotatorSimulation.AddTo(fixtureConfig,8,9);
            using var wheelServer = new HubFilterWheelServer();
            wheelServer.AddTo(fixtureConfig,4,17);
            var simulatedWheel=HubFilterWheelSimulation.AddTo(fixtureConfig,8,9);
            File.WriteAllText(args[1], fixtureConfig.ToJsonString());
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(75));
            var initializedPath = System.IO.Path.Combine(System.IO.Path.GetDirectoryName(args[1])!, "created empty configuration.json");
            var initialization = new HubInitialization(args[0]);
            await initialization.CreateAsync(initializedPath, deadline.Token);
            if (initialization.State != HubInitializationState.Created || initialization.InstanceId is null)
                throw new InvalidOperationException("Native initialization did not identify its empty file");
            var originalInstance = initialization.InstanceId;
            try { await initialization.CreateAsync(initializedPath, deadline.Token); throw new Exception("Initialization replaced an existing file"); }
            catch (HubException) { }
            if (!initialization.RequiresReconciliation) throw new InvalidOperationException("Creation error did not require file reconciliation");
            await initialization.ReadAsync(deadline.Token);
            if (initialization.State != HubInitializationState.Existing || initialization.InstanceId != originalInstance)
                throw new InvalidOperationException("File reconciliation changed initialized identity");
            var attachment = await HubAttachment.AttachAsync(args[0], args[1], cancellation: deadline.Token);
            candidate = attachment.StartedProcessId;
            if (candidate is null) throw new InvalidOperationException("Fixture expected to launch its unique test host");
            using var first = await HubClient.ConnectAsync(attachment, cancellation: deadline.Token);
            var attached = await HubAttachment.AttachAsync(args[0], args[1], cancellation: deadline.Token);
            if (attached.StartedProcessId is not null || attached.HostInstance != attachment.HostInstance)
                throw new InvalidOperationException("Shared host identity changed");
            using var second = await HubClient.ConnectAsync(attached, cancellation: deadline.Token);
            if (first.Hello.ClientId == second.Hello.ClientId) throw new InvalidOperationException("Clients were not independent");
            var saved = await first.RequestAsync(JsonSerializer.SerializeToElement(new { op = "getConfig" }), deadline.Token);
            var output = saved.GetProperty("outputs")[0].GetProperty("id").GetGuid();
            var outputStatus = JsonSerializer.SerializeToElement(new { op = "outputStatus", output,
                expectedRevision = saved.GetProperty("revision").GetGuid(), start = 0, limit = 1 });
            var diagnostic = await first.RequestAsync(outputStatus, deadline.Token);
            if (diagnostic.GetProperty("purpose").GetString() != "cachedDiagnostics" ||
                diagnostic.GetProperty("output").GetGuid() != output ||
                diagnostic.GetProperty("configurationRevision").GetGuid() != saved.GetProperty("revision").GetGuid() ||
                diagnostic.GetProperty("nextStart").GetInt32() != 1 ||
                diagnostic.GetProperty("diagnostics").GetProperty("channels")[0].GetProperty("health").GetProperty("leaseCount").GetInt32() != 0)
                throw new InvalidOperationException("Cached output diagnostics changed identity, pagination or acquired equipment");
            var connect = JsonSerializer.SerializeToElement(new { op = "changeConnection", output, connected = true, asynchronous = false });
            await first.RequestAsync(connect, deadline.Token); await second.RequestAsync(connect, deadline.Token);
            diagnostic = await second.RequestAsync(outputStatus, deadline.Token);
            if (diagnostic.GetProperty("diagnostics").GetProperty("channels")[0].GetProperty("health").GetProperty("leaseCount").GetInt32() != 2)
                throw new InvalidOperationException("Output diagnostics changed the independent client leases");
            var connected = JsonSerializer.SerializeToElement(new { op = "get", output, property = new { member = "connected" } });
            first.Dispose();
            if (!(await second.RequestAsync(connected, deadline.Token)).GetBoolean()) throw new InvalidOperationException("One client revoked another");
            await second.RequestAsync(JsonSerializer.SerializeToElement(new { op = "changeConnection", output, connected = false, asynchronous = false }), deadline.Token);
            if ((await second.RequestAsync(connected, deadline.Token)).GetBoolean()) throw new InvalidOperationException("Disconnect retained this lease");
            second.Dispose();
            using var probe = await HubClient.ConnectAsync(attached, cancellation: deadline.Token);
            if (probe.Hello.HostInstance != attachment.HostInstance) throw new InvalidOperationException("Frontend disconnect stopped the host");
            var binding = new HubSelection { ConfigPath = args[1], InstanceId = attached.InstanceId, OutputId = output,
                DeviceType = "switch", Label = "net48 simulated output", Simulated = true };
            var store = new HubSelectionStore(System.IO.Path.Combine(System.IO.Path.GetDirectoryName(args[1])!, "bindings.json"));
            var bindings = store.Save(binding, Guid.Empty);
            if (store.Load().Bindings.Single().Id != binding.Id) throw new InvalidOperationException("Selection identity changed on disk");
            try { store.Save(binding, Guid.Empty); throw new Exception("Selection store accepted a stale revision"); }
            catch (InvalidOperationException) { }
            using var native = new HubNativeSession(args[0]);
            await native.ConnectAsync(bindings.Bindings.Single(), deadline.Token);
            var epoch = native.Epoch;
            if (!(await native.RequestAsync(epoch, connected, cancellation: deadline.Token)).GetBoolean())
                throw new InvalidOperationException("Native session did not acquire its output");
            native.Disconnect();
            try { await native.RequestAsync(epoch, connected, cancellation: deadline.Token); throw new Exception("Retired session accepted a getter"); }
            catch (InvalidOperationException) { }
            await native.ConnectAsync(binding, deadline.Token);
            if (epoch == native.Epoch || !native.Connected) throw new InvalidOperationException("Explicit reconnect did not replace the session epoch");
            native.Disconnect();
            await native.ConnectAsync(binding, deadline.Token);
            var removed = store.Remove(binding.InstanceId, binding.OutputId, bindings.Revision);
            if (store.Load().Bindings.Length != 0 || !native.Connected ||
                !(await native.RequestAsync(native.Epoch, connected, cancellation: deadline.Token)).GetBoolean())
                throw new InvalidOperationException("Saved-choice removal changed a connected output");
            try { store.Save(binding, bindings.Revision); throw new Exception("Removal accepted a stale selection save"); }
            catch (InvalidOperationException) { }
            store.Save(binding, removed.Revision);
            native.Disconnect();
            using (var attachedOnly = new HubNativeSession(args[0])) {
                await attachedOnly.AttachAsync(binding, deadline.Token);
                if (!attachedOnly.IsAttached || attachedOnly.Connected ||
                    (await attachedOnly.RequestAsync(attachedOnly.Epoch, connected, cancellation: deadline.Token)).GetBoolean())
                    throw new InvalidOperationException("Attach acquired equipment without an explicit connection request");
                await attachedOnly.RequestAsync(attachedOnly.Epoch, connect, TimeSpan.FromSeconds(35), deadline.Token);
                if (!(await attachedOnly.RequestAsync(attachedOnly.Epoch, connected, cancellation: deadline.Token)).GetBoolean())
                    throw new InvalidOperationException("Attached client could not change its connection explicitly");
                attachedOnly.Disconnect();
            }
            using var editor = await HubEditorSession.AttachAsync(args[0], args[1], attached.InstanceId, deadline.Token);
            await editor.ReloadAsync(deadline.Token);
            var oldRevision = editor.Draft!.Revision;
            var cachedOutput = await editor.OutputStatusAsync(output, 0, 1, deadline.Token);
            if (cachedOutput.GetProperty("purpose").GetString() != "cachedDiagnostics" ||
                editor.DiagnosticSnapshot().GetProperty("outputObservation").GetProperty("kind").GetString() != "cachedOutputHealth")
                throw new InvalidOperationException("Native cached output editor/export failed");
            editor.Draft.SetValue("/outputs/0/label", JsonSerializer.SerializeToElement("net48 edited simulation")); editor.Changed();
            if (!await editor.ReviewAsync(deadline.Token) || !editor.Draft.Preview().Contains("net48 edited simulation"))
                throw new InvalidOperationException("Native editor review failed");
            // Disconnect is local immediately; wait for the private pipe close
            // to drain its host lease before deliberately applying the draft.
            foreach (var source in saved.GetProperty("sources").EnumerateArray()) {
                var id = source.GetProperty("id").GetGuid();
                while ((await editor.SourceStatusAsync(id, deadline.Token)).GetProperty("leaseCount").GetInt32() != 0)
                    await Task.Delay(25, deadline.Token);
            }
            await editor.ApplyAsync(deadline.Token);
            if (editor.State != HubEditorState.Uncertain) throw new InvalidOperationException("Apply skipped reconciliation");
            try { await editor.ApplyAsync(deadline.Token); throw new Exception("Editor replayed an unreconciled Apply"); }
            catch (InvalidOperationException) { }
            await editor.ReloadAsync(deadline.Token);
            var deviceChoices = editor.Draft!.Description.Root.GetProperty("$defs").GetProperty("VirtualDevice");
            var proxyChoice = editor.Draft.Description.Variants(deviceChoices).Single(v => v.Kind == "proxy");
            if (!proxyChoice.Enabled || editor.Draft.InitialValue(proxyChoice.Schema).GetProperty("deviceType").GetString() != "focuser" ||
                !editor.Draft.Description.Choices(proxyChoice.Schema.GetProperty("properties").GetProperty("deviceType")).Where(v => v.Enabled).Select(v => v.Value).SequenceEqual(new[] { "focuser", "rotator", "filterwheel" }))
                throw new InvalidOperationException("Typed proxy setup did not restrict creation to the published classes");
            if (editor.Draft!.Revision == oldRevision || editor.Draft.Field("/outputs/0/label").Value!.Value.GetString() != "net48 edited simulation")
                throw new InvalidOperationException("Native editor did not reconcile saved changes");
            var inspectedSource = saved.GetProperty("sources")[0].GetProperty("id").GetGuid();
            var inspection = await editor.InspectSourceAsync(inspectedSource, 0, 2, deadline.Token);
            if (!inspection.GetProperty("simulation").GetBoolean() || inspection.GetProperty("capabilities").GetProperty("nextStart").GetInt32() != 2)
                throw new InvalidOperationException("Native setup inspection lost simulation or pagination");
            inspection = await editor.InspectSourceAsync(inspectedSource, 2, 2, deadline.Token);
            if (inspection.GetProperty("capabilities").GetProperty("channels").GetArrayLength() != 1 ||
                editor.DiagnosticSnapshot().GetProperty("observation").GetProperty("kind").GetString() != "setupInspection")
                throw new InvalidOperationException("Native setup inspection/export failed");
            while ((await editor.SourceStatusAsync(inspectedSource, deadline.Token)).GetProperty("leaseCount").GetInt32() != 0)
                await Task.Delay(25, deadline.Token);
            if (editor.SimulationControls(inspectedSource).Count != 5) throw new InvalidOperationException("Missing described simulation controls");
            var simulated = await editor.UpdateSimulationAsync(inspectedSource,JsonSerializer.SerializeToElement(new { switchValues = new Dictionary<string,double> { ["1"] = 23 } }),deadline.Token);
            if (simulated.GetProperty("simulation").GetProperty("switchValues").GetProperty("1").GetDouble()!=23 ||
                simulated.GetProperty("simulation").GetProperty("switchValues").GetProperty("2").GetDouble()!=12)
                throw new InvalidOperationException("Sparse simulation update changed another channel");
            // Restore this fixture's shared value before the independent ASCOM
            // contract suite checks the new-runtime default.
            await editor.UpdateSimulationAsync(inspectedSource,JsonSerializer.SerializeToElement(new { switchValues = new Dictionary<string,double> { ["1"] = 0 } }),deadline.Token);
            while ((await editor.SourceStatusAsync(inspectedSource, deadline.Token)).GetProperty("leaseCount").GetInt32() != 0)
                await Task.Delay(25, deadline.Token);
            await NativeOutputs.Run(args[0], args[1], attached.InstanceId, saved, probe, deadline.Token);
            await NativeOutputs.FocuserRun(args[0], args[1], attached.InstanceId, saved, focuserServer, deadline.Token);
            await NativeOutputs.SimulatedFocuserRun(args[0],args[1],attached.InstanceId,saved,simulatedFocuser,editor,deadline.Token);
            async Task RotatorCheckpoint(string stage, Guid source, Func<Task> run)
            {
                try { await run(); }
                catch {
                    // Preserve dispatch/reply evidence before another IPC round
                    // trip. Success adds no diagnostics, reads or retries.
                    var trace = rotatorServer.RequestTrace;
                    var moves = rotatorServer.Moves; var halts = rotatorServer.Halts;
                    ThreadPool.GetAvailableThreads(out var workers, out var completionPorts);
                    string state;
                    try { state = (await editor.SourceStatusAsync(source, deadline.Token)).GetRawText(); }
                    catch (Exception diagnostic) { state = "unavailable: " + diagnostic.Message; }
                    Console.Error.WriteLine($"{stage}: moves={moves}, halts={halts}, availableWorkers={workers}, " +
                        $"availableCompletionPorts={completionPorts}, source={state}, private request trace={trace}");
                    throw;
                }
            }
            await RotatorCheckpoint("ASCOM loopback rotator", rotatorServer.SourceId,
                () => NativeOutputs.RotatorRun(args[0],args[1],attached.InstanceId,saved,rotatorServer,deadline.Token));
            await RotatorCheckpoint("ASCOM simulated rotator", simulatedRotator,
                () => NativeOutputs.SimulatedRotatorRun(args[0],args[1],attached.InstanceId,saved,simulatedRotator,editor,deadline.Token));
            foreach (var deviceType in new[] { "rotator", "filterwheel" })
                await NativeOutputs.CreatedTypedRun(args[0],args[1],attached.InstanceId,editor,deviceType,deadline.Token);
            await NativeOutputs.WheelRun(args[0],args[1],attached.InstanceId,saved,wheelServer,deadline.Token);
            await NativeOutputs.SimulatedWheelRun(args[0],args[1],attached.InstanceId,saved,simulatedWheel,editor,deadline.Token);
            Console.WriteLine($"net48 {IntPtr.Size * 8}-bit: shared identity, independent leases, selection CAS/removal, native session/reconnect, editor review/apply/reconcile, setup inspection/export/simulation, typed ASCOM outputs and surviving host passed");
            return 0;
        } catch (Exception error) { Console.Error.WriteLine(error.ToString()); return 1; }
        finally {
            // Test-only cleanup, never used by production frontends. The script
            // supplies a fresh simulation-only config and launches sequentially.
            if (candidate is uint pid) {
                try {
                    using var host = Process.GetProcessById(checked((int)pid));
                    var path = new StringBuilder(32768); uint length = (uint)path.Capacity;
                    if (!QueryFullProcessImageName(host.Handle, 0, path, ref length) ||
                        !string.Equals(System.IO.Path.GetFullPath(args[0]), path.ToString(), StringComparison.OrdinalIgnoreCase))
                        throw new InvalidOperationException("Fixture host executable did not match; refusing cleanup");
                    host.Kill();
                    if (!host.WaitForExit(5000)) throw new InvalidOperationException("Fixture cleanup timed out");
                } catch (ArgumentException) { }
            }
        }
    }
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool QueryFullProcessImageName(IntPtr process, uint flags, StringBuilder path, ref uint length);
}
