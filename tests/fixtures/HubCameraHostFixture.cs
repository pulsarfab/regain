using System.Diagnostics;
using System.IO;
using System.Text;
using System.Text.Json;
using Regain.Hub;

namespace Regain.TestFixtures;

internal static class HubCameraHostFixture
{
    private static void Check(bool value, string message)
    { if (!value) throw new InvalidOperationException(message); }
    // Explicit --simulate is mandatory here. SDK path deliberately does not exist.
    // Start/kill only this private process; attaching clients never own the host.
    internal static async Task Run(string executable, bool direct)
    {
        executable = Path.GetFullPath(executable);
        var workers = Path.GetDirectoryName(executable)!;
        Check(File.Exists(executable), "Build the hub executable before testing camera IPC");
        var directory = Path.Combine(Path.GetTempPath(), "Regain image simulation " + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        var configPath = Path.Combine(directory, "camera.json");
        var instance = Guid.NewGuid(); var revision = Guid.NewGuid(); var source = Guid.NewGuid();
        var firstOutput = Guid.NewGuid(); var secondOutput = Guid.NewGuid();
        var configuration = new {
            schemaVersion = 1, instanceId = instance, revision,
            sources = new[] { new { id = source, label = "Private camera simulation", backend = new {
                kind = "native", device = direct ? "camera-direct" : "camera-sdk", identity = direct ? "direct-simulator" : "sim00001",
                camera = new { model = direct ? "ZWO ASI585MM Pro" : "ZWO Simulated",
                    recovery = new { maxRetries = 0, readyFrameDownloadRetries = 0, reconnectDelaySeconds = 0.01 } } },
                polling = new { connectionTimeoutSeconds = 10, requestTimeoutSeconds = 5, pollSeconds = 60 } } },
            outputs = new[] { new { id = firstOutput, number = 2, label = "Private camera A", device = new { kind = "proxy", source, deviceType = "camera" } },
                new { id = secondOutput, number = 9, label = "Private camera B", device = new { kind = "proxy", source, deviceType = "camera" } } }
        };
        File.WriteAllText(configPath, JsonSerializer.Serialize(configuration), new UTF8Encoding(false));
        using var process = new Process { StartInfo = new ProcessStartInfo(executable,
            "--hub-host --simulate --hub-config " + HubAttachment.Quote(configPath) + " --workers " + HubAttachment.Quote(workers) +
            " --sdk " + HubAttachment.Quote(Path.Combine(directory, "absent-sdk.dll"))) {
            UseShellExecute = false, CreateNoWindow = true, WindowStyle = ProcessWindowStyle.Hidden,
            RedirectStandardOutput = true, RedirectStandardError = true } };
        var started = false;
        var completed = false;
        Task<string>? errors = null;
        var budget = new HubImageBudget(4 * 1024 * 1024);
        try {
            Check(process.Start(), "Private camera host failed to start"); started = true;
            errors = process.StandardError.ReadToEndAsync();
            var readiness = process.StandardOutput.ReadLineAsync();
            if (await Task.WhenAny(readiness, Task.Delay(10000)) != readiness) throw new TimeoutException("Private camera host readiness expired");
            Check((await readiness)?.StartsWith("Regain hub ready:", StringComparison.Ordinal) == true, "Private camera host not ready");
            using var stop = new CancellationTokenSource(TimeSpan.FromSeconds(45));
            var attached = await HubAttachment.AttachAsync(executable, configPath, workers, stop.Token, instance);
            Check(attached.StartedProcessId is null, "Attachment launched a replacement simulation host");
            using var control = await HubClient.ConnectAsync(attached, cancellation: stop.Token);
            using var observer = await HubClient.ConnectAsync(attached, cancellation: stop.Token);
            async Task<JsonElement> Command(HubClient client, object value) => await client.RequestAsync(JsonSerializer.SerializeToElement(value), stop.Token);
            async Task<JsonElement> CameraCommand(HubClient client, HubCameraTiming timing, object value) =>
                await client.RequestCameraAsync(timing, JsonSerializer.SerializeToElement(value), stop.Token);
            var firstTiming = await control.GetCameraTimingAsync(firstOutput, stop.Token);
            var secondTiming = await observer.GetCameraTimingAsync(secondOutput, stop.Token);
            Check(firstTiming.Native && firstTiming.Source == source && firstTiming.Connect > TimeSpan.FromSeconds(35), "Native controller deadline not negotiated");
            try {
                await CameraCommand(observer, firstTiming, new { op = "connect", output = firstOutput });
                throw new InvalidOperationException("Another client's timing descriptor was accepted");
            } catch (HubException error) { Check(error.Failure == HubFailure.InvalidRequest, "Cross-client descriptor rejection changed"); }
            var before = await Command(control, new { op = "sourceStatus", source });
            Check(before.GetProperty("leaseCount").GetInt32() == 0, "Timing negotiation acquired equipment");
            await CameraCommand(control, firstTiming, new { op = "connect", output = firstOutput });
            await CameraCommand(observer, secondTiming, new { op = "connect", output = secondOutput });
            foreach (var property in new[] { "numX", "numY" }) await CameraCommand(control, firstTiming, new {
                op = "put", output = firstOutput, property = new { member = "cameraSetting", setting = new { property, value = 256 } } });
            async Task<HubImageRequest> Capture(HubClient client, HubCameraTiming timing) {
                var output = timing.Output;
                await CameraCommand(client, timing, new { op = "put", output, property = new { member = "startExposure", request = new { durationSeconds = 0.05, light = false } } });
                var clock = Stopwatch.StartNew();
                while (clock.Elapsed < TimeSpan.FromSeconds(15)) {
                    var status = await Command(client, new { op = "get", output, property = new { member = "cameraAcquisition" } });
                    if (status.GetProperty("imageReady").GetBoolean()) {
                        var completed = status.GetProperty("completed");
                        return new HubImageRequest(client.Hello.HostInstance, revision, client.Hello.ClientId, output, source,
                            completed.GetProperty("generation").GetGuid(), completed.GetProperty("acquisition").GetGuid());
                    }
                    await Task.Delay(5, stop.Token);
                }
                throw new TimeoutException("Private camera simulation did not publish its image");
            }
            var request = await Capture(control, firstTiming);
            using var first = await HubCameraImages.DownloadAsync(attached, control, request, budget, TimeSpan.FromSeconds(10), stop.Token);
            using var second = await HubCameraImages.DownloadAsync(attached, control, request, budget, TimeSpan.FromSeconds(10), stop.Token);
            Check(first.Descriptor.ElementType == HubImageElementType.Int32 && first.Descriptor.TransmissionType == HubImageElementType.UInt16 &&
                first.ByteLength == 256 * 256 * 2 && first.Descriptor.Rank == 2, "Rust/managed image descriptor disagrees");
            var original = new byte[257]; var repeated = new byte[257];
            first.CopyTo(65530, original, 0, original.Length); second.CopyTo(65530, repeated, 0, repeated.Length);
            Check(original.SequenceEqual(repeated), "Repeated immutable read changed pixels");
            Check(budget.UsedBytes == 2 * first.ByteLength, "Independent downloads not charged separately");
            var snapshot = await Command(control, new { op = "sourceStatus", source });
            Check(snapshot.GetProperty("leaseCount").GetInt32() == 2, "Image download acquired an equipment lease");
            Check((await Command(control, new { op = "get", output = firstOutput, property = new { member = "connected" } })).GetBoolean(), "Image EOF retired control client");
            var tiny = new HubImageBudget(1);
            try {
                using var unexpected = await HubCameraImages.DownloadAsync(attached, control, request, tiny, TimeSpan.FromSeconds(10), stop.Token);
                throw new InvalidOperationException("Insufficient receiver capacity accepted");
            } catch (HubException error) { Check(error.Failure == HubFailure.Busy && tiny.UsedBytes == 0, "Receiver capacity failure changed"); }
            using var pin = first.Pin(); first.Dispose(); second.Dispose();
            Check(budget.UsedBytes == first.ByteLength, "Pin changed accounting");
            var newer = await Capture(observer, secondTiming);
            Check(newer.Acquisition != request.Acquisition, "New capture reused acquisition identity");
            using (var replacement = await HubCameraImages.DownloadAsync(attached, observer, newer, budget, TimeSpan.FromSeconds(10), stop.Token)) {
                pin.CopyTo(65530, repeated, 0, repeated.Length); Check(original.SequenceEqual(repeated), "Next capture changed a pinned image");
            }
            try {
                using var stale = await HubCameraImages.DownloadAsync(attached, control, request, budget, TimeSpan.FromSeconds(10), stop.Token);
                throw new InvalidOperationException("Old acquisition downloaded after replacement");
            } catch (HubException error) { Check(error.Failure == HubFailure.Remote, "Stale acquisition failure changed"); }
            Check(control.IsConnected && observer.IsConnected, "Image rejection retired a control client");
            control.Dispose();
            Check((await Command(observer, new { op = "get", output = secondOutput, property = new { member = "connected" } })).GetBoolean(), "Client loss revoked sibling");
            pin.CopyTo(65530, repeated, 0, repeated.Length); Check(original.SequenceEqual(repeated), "Client loss changed pinned pixels");
            pin.Dispose(); Check(budget.UsedBytes == 0, "Completed managed images leaked budget");
            await Command(observer, new { op = "disconnect", output = secondOutput });
            Console.WriteLine($"Camera image {(direct ? "direct" : "SDK")} simulation {IntPtr.Size * 8}-bit: protected pipe, multichunk bytes, independent readers, retained pins, budget and identity rejection passed");
            completed = true;
        } finally {
            // The Process object is the specific child started above, never a
            // PID supplied by an attachment or an installed equipment process.
            if (started && !process.HasExited) { process.Kill(); if (!process.WaitForExit(10000)) throw new TimeoutException("Private camera host did not exit"); }
            if (errors is not null) {
                var trace = await errors;
                if (!completed && trace.Length != 0) Console.Error.WriteLine(trace);
            }
            Directory.Delete(directory, true);
        }
        Check(budget.UsedBytes == 0, "Private fixture leaked image budget");
    }
}
