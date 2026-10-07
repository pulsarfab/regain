using System.Collections.Concurrent;
using System.Diagnostics;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using Moq;
using NINA.Core.Model.Equipment;
using NINA.Equipment.Model;
using NINA.Image.Interfaces;
using NINA.Profile.Interfaces;
using Regain.Hub;
using Regain.TestFixtures;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class HubCameraAlpacaTests
{
    [Theory, InlineData("none"), InlineData("cancel"), InlineData("duration"), InlineData("startTime"),
        InlineData("geometry"), InlineData("pixels"), InlineData("ack")]
    public async Task ActualAlpacaCameraProxyPreservesCaptureOrFailsWithoutReplay(string fault)
    {
        var root = new DirectoryInfo(AppContext.BaseDirectory);
        while (root is not null && !File.Exists(Path.Combine(root.FullName, "Cargo.toml"))) root = root.Parent;
        var workers = Environment.GetEnvironmentVariable("REGAIN_TEST_WORKERS") ?? Path.Combine(root!.FullName, "target", "debug");
        var executable = Path.Combine(workers, "regain-alpaca.exe");
        await HubCameraHostFixture.Run(executable, false, true, false, async (upstream, _) => {
            var directory = Path.GetDirectoryName(upstream.ConfigPath)!;
            var profilesFile = Path.Combine(directory, "empty-http-profiles.json");
            await File.WriteAllTextAsync(profilesFile, "[]");
            using var portReservation = new TcpListener(IPAddress.Loopback, 0);
            portReservation.Start(); var port = ((IPEndPoint)portReservation.LocalEndpoint).Port; portReservation.Stop();
            using var publisher = Child(executable, "--hub-config", upstream.ConfigPath, "--profiles", profilesFile,
                "--simulate", "--workers", workers, "--no-discovery", "--port", port.ToString());
            Assert.True(publisher.Start());
            var publisherOutput = publisher.StandardOutput.ReadToEndAsync(); var publisherError = publisher.StandardError.ReadToEndAsync();
            Process? downstream = null; Task<string>? downstreamOutput = null, downstreamError = null;
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(40));
            try {
                using var readiness = new HttpClient { BaseAddress = new Uri("http://127.0.0.1:" + port), Timeout = TimeSpan.FromSeconds(1) };
                var started = Stopwatch.StartNew();
                while (true) {
                    if (publisher.HasExited) throw new IOException("Private camera publisher exited: " + await publisherError);
                    try { using var response = await readiness.GetAsync("/management/v1/configureddevices", deadline.Token); response.EnsureSuccessStatusCode(); break; }
                    catch (HttpRequestException) when (started.Elapsed < TimeSpan.FromSeconds(10)) { await Task.Delay(10, deadline.Token); }
                    catch (OperationCanceledException) when (!deadline.IsCancellationRequested && started.Elapsed < TimeSpan.FromSeconds(10)) {
                        // Startup probes are read-only. A busy CI publisher may
                        // time out its first HTTP reply before it is ready;
                        // retry only within the existing finite startup window.
                        await Task.Delay(10, deadline.Token);
                    }
                }
                using var relay = new CameraRelay(readiness.BaseAddress!, fault);
                var instance = Guid.NewGuid(); var source = Guid.NewGuid(); var output = Guid.NewGuid(); var second = Guid.NewGuid();
                var configPath = Path.Combine(directory, "network-camera.json");
                await File.WriteAllTextAsync(configPath, JsonSerializer.Serialize(new {
                    schemaVersion = 1, instanceId = instance, revision = Guid.NewGuid(),
                    sources = new[] { new { id = source, label = "Private loopback camera", backend = new {
                        kind = "alpaca", baseUrl = relay.Url, deviceType = "camera", deviceNumber = 2, connectionPolicy = "managed" },
                        polling = new { pollSeconds = 60, requestTimeoutSeconds = 1, connectionTimeoutSeconds = 10 } } },
                    outputs = new[] { new { id = output, number = 4, label = "Private network camera A", device = new { kind = "proxy", source, deviceType = "camera" } },
                        new { id = second, number = 7, label = "Private network camera B", device = new { kind = "proxy", source, deviceType = "camera" } } }
                }));
                downstream = Child(executable, "--hub-host", "--hub-config", configPath, "--workers", workers, "--simulate");
                Assert.True(downstream.Start()); downstreamError = downstream.StandardError.ReadToEndAsync();
                Assert.StartsWith("Regain hub ready:", await downstream.StandardOutput.ReadLineAsync(deadline.Token));
                downstreamOutput = downstream.StandardOutput.ReadToEndAsync();
                var attachment = await HubAttachment.AttachAsync(executable, configPath, workers, deadline.Token, instance);
                Assert.Null(attachment.StartedProcessId);
                using var control = await HubClient.ConnectAsync(attachment, cancellation: deadline.Token);
                HubSelection Binding(Guid id) => new() { ConfigPath = configPath, InstanceId = instance, OutputId = id,
                    DeviceType = "camera", Label = "Private loopback camera" };
                var settings = new Mock<ICameraSettings>(); settings.SetupProperty(value => value.Timeout, 1);
                var profiles = new Mock<IProfileService>(); profiles.Setup(value => value.ActiveProfile.CameraSettings).Returns(settings.Object);
                using var owner = new HubCameraDevice(Binding(output), Mock.Of<IImageDataFactory>(), profiles.Object, executable, workers);
                using var sibling = new HubCameraDevice(Binding(second), Mock.Of<IImageDataFactory>(), executable: executable, workers: workers);
                await owner.Connect(deadline.Token); await sibling.Connect(deadline.Token);
                var commands = await control.GetCameraTimingAsync(output, deadline.Token);
                await control.RequestCameraAsync(commands, JsonSerializer.SerializeToElement(new { op = "connect", output }), deadline.Token);
                var timing = await control.GetCameraCaptureTimingAsync(output, 0.1, deadline.Token);
                Assert.False(timing.Native); // A network proxy never gains native reread/recovery policy.
                owner.EnableSubSample = true; owner.SubSampleWidth = 96; owner.SubSampleHeight = 64;
                var sequence = new CaptureSequence { ExposureTime = 0.1, Binning = new BinningMode(1, 1), Gain = -1, Offset = -1 };
                if (fault == "ack") {
                    var rejected = Assert.Throws<HubException>(() => owner.StartExposure(sequence));
                    Assert.True(rejected.Failure is HubFailure.Remote or HubFailure.Uncertain);
                    Assert.Equal(1, settings.Object.Timeout);
                    var uncertain = await control.RequestAsync(JsonSerializer.SerializeToElement(new { op = "get", output,
                        property = new { member = "cameraAcquisition" } }), deadline.Token);
                    Assert.Equal("uncertain", uncertain.GetProperty("phase").GetString());
                    Assert.NotEqual(JsonValueKind.Null, uncertain.GetProperty("acquisition").ValueKind);
                    Assert.NotEqual(JsonValueKind.Null, uncertain.GetProperty("owner").ValueKind);
                    Assert.Throws<HubException>(() => owner.StartExposure(sequence));
                } else {
                    owner.StartExposure(sequence);
                    if (fault == "cancel") {
                        await relay.ImageEntered.Task.WaitAsync(deadline.Token);
                        using var cancel = new CancellationTokenSource();
                        var waiting = owner.WaitUntilExposureIsReady(cancel.Token); Assert.False(waiting.IsCompleted);
                        cancel.Cancel(); await Assert.ThrowsAnyAsync<OperationCanceledException>(() => waiting);
                        Assert.Equal(1, settings.Object.Timeout);
                        Assert.True(sibling.Connected); relay.ReleaseImage.Set();
                    }
                    if (fault is "geometry" or "pixels" or "duration" or "startTime") {
                        await Assert.ThrowsAsync<IOException>(() => owner.WaitUntilExposureIsReady(deadline.Token));
                        Assert.Equal(1, settings.Object.Timeout);
                        var status = await control.RequestAsync(JsonSerializer.SerializeToElement(new { op = "get", output,
                            property = new { member = "cameraAcquisition" } }), deadline.Token);
                        Assert.Equal("uncertain", status.GetProperty("phase").GetString());
                        Assert.False(status.GetProperty("imageReady").GetBoolean());
                        Assert.NotEqual(JsonValueKind.Null, status.GetProperty("error").ValueKind);
                    } else {
                        await owner.WaitUntilExposureIsReady(deadline.Token);
                        var frame = Assert.IsType<HubCameraExposureData>(await owner.DownloadExposure(deadline.Token));
                        Assert.Equal(96, frame.Width); Assert.Equal(64, frame.Height);
                        Assert.Equal(0.1, frame.MetaData.Image.ExposureTime, 5);
                        Assert.IsType<ushort[]>(frame.Pixels);
                        owner.Disconnect(); Assert.True(sibling.Connected);
                        Assert.Equal(96 * 64, frame.Pixels.Length);
                        Assert.Equal(1, settings.Object.Timeout);
                    }
                }
                Assert.Equal(1, relay.Commands.Count(command => command == "PUT startexposure"));
                Assert.DoesNotContain("PUT abortexposure", relay.Commands);
                Assert.DoesNotContain("PUT stopexposure", relay.Commands);
                Assert.Equal(fault is "ack" or "duration" or "startTime" ? 0 : 1,
                    relay.Commands.Count(command => command == "GET imagearray"));
            } finally {
                if (downstream is not null) {
                    if (!downstream.HasExited) { downstream.Kill(); await downstream.WaitForExitAsync(); }
                    if (downstreamOutput is not null) await downstreamOutput;
                    if (downstreamError is not null) Console.WriteLine(await downstreamError);
                    downstream.Dispose();
                }
                if (!publisher.HasExited) { publisher.Kill(); await publisher.WaitForExitAsync(); }
                await publisherOutput; Console.WriteLine(await publisherError);
            }
        });
    }
    private static Process Child(string executable, params string[] args) {
        var process = new Process { StartInfo = new ProcessStartInfo(executable) { UseShellExecute = false, CreateNoWindow = true,
            WindowStyle = ProcessWindowStyle.Hidden, RedirectStandardOutput = true, RedirectStandardError = true } };
        foreach (var arg in args) process.StartInfo.ArgumentList.Add(arg);
        return process;
    }

    // Transparent private upstream fault relay. Each blocking handler has its
    // own thread; synchronous NINA calls cannot starve response production.
    private sealed class CameraRelay : IDisposable
    {
        private readonly TcpListener listener = new(IPAddress.Loopback, 0);
        private readonly HttpClient http;
        private readonly CancellationTokenSource stopping = new();
        private readonly BlockingCollection<TcpClient> pending = new(32);
        private readonly ConcurrentDictionary<TcpClient, byte> clients = new();
        private readonly Task accept;
        private readonly Task[] handlers;
        private readonly string fault;
        internal readonly ConcurrentQueue<string> Commands = new();
        internal readonly TaskCompletionSource<bool> ImageEntered = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal readonly ManualResetEventSlim ReleaseImage = new();
        internal string Url { get; }
        internal CameraRelay(Uri upstream, string fault) {
            this.fault = fault; http = new HttpClient { BaseAddress = upstream, Timeout = TimeSpan.FromSeconds(15), MaxResponseContentBufferSize = 8 * 1024 * 1024 };
            listener.Start(); Url = "http://127.0.0.1:" + ((IPEndPoint)listener.LocalEndpoint).Port;
            handlers = Enumerable.Range(0, 4).Select(_ => Task.Factory.StartNew(HandleAll, CancellationToken.None, TaskCreationOptions.LongRunning, TaskScheduler.Default)).ToArray();
            accept = Task.Factory.StartNew(() => {
                try { while (!stopping.IsCancellationRequested) {
                    var client = listener.AcceptTcpClient(); client.NoDelay = true; clients.TryAdd(client, 0); pending.Add(client, stopping.Token);
                } } catch (Exception error) when (stopping.IsCancellationRequested && error is SocketException or ObjectDisposedException or OperationCanceledException) { }
            }, CancellationToken.None, TaskCreationOptions.LongRunning, TaskScheduler.Default);
        }
        private void HandleAll() {
            foreach (var client in pending.GetConsumingEnumerable()) {
                try { Forward(client); }
                catch (Exception error) when (error is IOException or SocketException or ObjectDisposedException or OperationCanceledException) { }
                finally { clients.TryRemove(client, out _); client.Dispose(); }
            }
        }
        private void Forward(TcpClient client) {
            using var stream = client.GetStream(); using var reader = new StreamReader(stream, Encoding.ASCII, false, 4096, true);
            var first = reader.ReadLine()?.Split(' '); if (first is null) return;
            var length = 0; string? accepted = null;
            for (var line = reader.ReadLine(); !string.IsNullOrEmpty(line); line = reader.ReadLine()) {
                if (line.StartsWith("Content-Length:", StringComparison.OrdinalIgnoreCase)) length = int.Parse(line.Substring(15).Trim());
                if (line.StartsWith("Accept:", StringComparison.OrdinalIgnoreCase)) accepted = line.Substring(7).Trim();
            }
            if (length > 65536 || length < 0) throw new IOException("Unbounded fixture request");
            var body = new char[length]; var offset = 0;
            while (offset < length) { var count = reader.Read(body, offset, length - offset); if (count == 0) throw new IOException("Truncated request"); offset += count; }
            var member = new Uri("http://fixture" + first[1]).Segments.Last();
            Commands.Enqueue(first[0] + " " + member);
            using var request = new HttpRequestMessage(new HttpMethod(first[0]), first[1]);
            if (accepted is not null) request.Headers.TryAddWithoutValidation("Accept", accepted);
            if (first[0] == "PUT") request.Content = new StringContent(new string(body), Encoding.ASCII, "application/x-www-form-urlencoded");
            using var response = http.SendAsync(request, stopping.Token).GetAwaiter().GetResult();
            var bytes = response.Content.ReadAsByteArrayAsync(stopping.Token).GetAwaiter().GetResult();
            response.EnsureSuccessStatusCode();
            if (fault == "ack" && first[0] == "PUT" && member == "startexposure") return;
            if (member == "imagearray") {
                ImageEntered.TrySetResult(true);
                if (fault == "cancel") ReleaseImage.Wait(stopping.Token);
                if (fault == "pixels") bytes = bytes.Take(bytes.Length - 1).ToArray();
            }
            if (first[0] == "GET" && (fault == "geometry" && member == "numx" || fault == "duration" && member == "lastexposureduration"
                || fault == "startTime" && member == "lastexposurestarttime")) {
                var json = JsonNode.Parse(bytes)!.AsObject();
                json["Value"] = fault == "geometry" ? JsonValue.Create(json["Value"]!.GetValue<int>() + 8) : JsonValue.Create("invalid metadata");
                bytes = JsonSerializer.SerializeToUtf8Bytes(json);
            }
            var header = Encoding.ASCII.GetBytes("HTTP/1.1 200 OK\r\nContent-Type: " + response.Content.Headers.ContentType +
                "\r\nContent-Length: " + bytes.Length + "\r\nConnection: close\r\n\r\n");
            stream.Write(header); stream.Write(bytes);
        }
        public void Dispose() {
            stopping.Cancel(); ReleaseImage.Set(); listener.Stop();
            try { accept.GetAwaiter().GetResult(); }
            finally {
                foreach (var client in clients.Keys) client.Dispose();
                pending.CompleteAdding(); Task.WhenAll(handlers).GetAwaiter().GetResult();
                pending.Dispose(); ReleaseImage.Dispose(); http.Dispose(); stopping.Dispose();
            }
        }
    }
}
