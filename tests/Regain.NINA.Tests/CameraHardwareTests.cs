using System.Diagnostics;
using System.Text.Json;
using Moq;
using NINA.Core.Model.Equipment;
using NINA.Equipment.Model;
using NINA.Image.ImageData;
using NINA.Image.Interfaces;
using Regain.Core;
using Xunit;
using Xunit.Abstractions;

namespace Regain.NINA.Tests;

public sealed class CameraHardwareTheoryAttribute : TheoryAttribute
{
    public CameraHardwareTheoryAttribute() {
        if (Environment.GetEnvironmentVariable("REGAIN_TEST_ASI585_HARDWARE") != "1")
            Skip = "Opt in with REGAIN_TEST_ASI585_HARDWARE=1; requires an idle physical ASI585MM Pro";
    }
}

public sealed class Asi6200HardwareFactAttribute : FactAttribute
{
    public Asi6200HardwareFactAttribute() {
        if (Environment.GetEnvironmentVariable("REGAIN_TEST_ASI6200_HARDWARE") != "1")
            Skip = "Opt in with REGAIN_TEST_ASI6200_HARDWARE=1; requires idle ASI6200MM Pro serial 1b07450d17010900";
    }
}

public class CameraHardwareTests(ITestOutputHelper output)
{
    [CameraHardwareTheory]
    [InlineData(true, false)]
    [InlineData(false, false)]
    [InlineData(true, true)]
    [InlineData(false, true)]
    public async Task Asi585DisconnectDisablesThermalControls(bool direct, bool exposing)
    {
        string root = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../"));
        string sdk = Path.Combine(root, "vendor/zwo/ASICamera2.dll");
        HostClient? host = null;
        var camera = new ResilientCamera(new("ZWO ASI585MM Pro", 3840, 2160, false, 0, 2.9, 12, true, false, [1, 2, 3, 4]),
            Mock.Of<IExposureDataFactory>(), () => host = new HostClient(Path.Combine(root, "target/debug/regain-alpaca.exe"), sdk,
                direct: direct, supervised: true, log: output.WriteLine), new());
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(45));
        try {
            Assert.True(await camera.Connect(deadline.Token));
            var identity = (await host!.CallAsync("diagnostics", null, TimeSpan.FromSeconds(10), deadline.Token)).Result;
            Assert.Equal("2805960a19020900", identity.GetProperty("serial").GetString());
            // This model advertises no controllable dew heater. Heater shutdown
            // is covered by capability/failure and ASI6200 simulated tests.
            Assert.False(identity.GetProperty("controls").TryGetProperty("21", out _));
            camera.TemperatureSetPoint = 5;
            camera.CoolerOn = true;
            await Task.Delay(2500, deadline.Token);
            Assert.True(camera.CoolerOn);
            Task? ready = null;
            if (exposing) {
                camera.EnableSubSample = true;
                camera.SubSampleWidth = camera.SubSampleHeight = 64;
                camera.StartExposure(Sequence(600));
                ready = camera.WaitUntilExposureIsReady(deadline.Token);
                while ((await host.CallAsync("diagnostics", null, TimeSpan.FromSeconds(10), deadline.Token))
                    .Result.GetProperty("phase").GetString() != "Exposing")
                    await Task.Delay(25, deadline.Token);
            }
            camera.Disconnect();
            Assert.False(camera.Connected);
            if (ready is not null) {
                var error = await Record.ExceptionAsync(() => ready.WaitAsync(TimeSpan.FromSeconds(10)));
                Assert.True(error is IOException or OperationCanceledException, $"Unexpected disconnect result: {error}");
            }
            // Inspect the actual enable flag through direct USB, even after an
            // SDK disconnect. Do not use SDK open defaults as hardware evidence.
            using var probe = new HostClient(Path.Combine(root, "target/debug/regain-device.exe"), sdk, direct: true, log: output.WriteLine);
            await probe.CallAsync("open", new { name = "ZWO ASI585MM Pro", serial = "2805960a19020900" }, TimeSpan.FromSeconds(10), deadline.Token);
            Assert.Equal(0, (await probe.CallAsync("get", new { control = 17 }, TimeSpan.FromSeconds(10), deadline.Token)).Result.GetInt64());
            await probe.CallAsync("close", null, TimeSpan.FromSeconds(10), deadline.Token);
            output.WriteLine($"Disconnect thermal shutdown verified through USB enable flag after {(direct ? "direct" : "sdk")} NINA session; exposing={exposing}");
        }
        finally { camera.Disconnect(); }
    }

    [CameraHardwareTheory]
    [InlineData(true)]
    [InlineData(false)]
    public async Task Asi585LiveCoolingAbortAndRestartThroughNina(bool direct)
    {
        string root = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../"));
        HostClient? host = null;
        ImageMetaData? delivered = null;
        var images = new Mock<IExposureDataFactory>();
        images.Setup(f => f.CreateImageArrayExposureData(It.IsAny<ushort[]>(), It.IsAny<int>(), It.IsAny<int>(), It.IsAny<int>(), It.IsAny<bool>(), It.IsAny<ImageMetaData>()))
            .Returns((ushort[] p, int w, int h, int bits, bool color, ImageMetaData m) => {
                delivered = m;
                output.WriteLine($"Image: {w}x{h}, mean {p.Average(v => (double)v):F2}, model {m.Camera.Name}");
                return new ImageArrayExposureData(p, w, h, bits, color, m, Mock.Of<IImageDataFactory>());
            });
        var camera = new ResilientCamera(new("ZWO ASI585MM Pro", 3840, 2160, false, 0, 2.9, 12, true, false, [1, 2, 3, 4]), images.Object,
            () => host = new HostClient(Path.Combine(root, "target/debug/regain-alpaca.exe"), Path.Combine(root, "vendor/zwo/ASICamera2.dll"),
                direct: direct, supervised: true, log: line => output.WriteLine(line)),
            new() { MaxRetries = 1, ReconnectDelaySeconds = 1, CoolingTimeoutSeconds = 180 });
        using var deadline = new CancellationTokenSource(TimeSpan.FromMinutes(4));
        try {
            Assert.True(await camera.Connect(deadline.Token));
            var initial = await Diagnostics();
            string serial = initial.GetProperty("serial").GetString()!;
            Assert.Equal("2805960a19020900", serial);
            Assert.Equal(direct ? "direct" : "sdk", initial.GetProperty("backend").GetString());
            output.WriteLine($"Hardware: {camera.Name}, serial {serial}, backend {(direct ? "direct" : "sdk")}");
            int pid = initial.GetProperty("processId").GetInt32();
            // SDK open values are defaults. Allow the two-second hardware poll
            // to supply measured telemetry before setting a relative target.
            await Task.Delay(3000, deadline.Token);
            double temperature = (await host!.CallAsync("get", new { control = 8 }, TimeSpan.FromSeconds(15), deadline.Token)).Result.GetInt64() / 10.0;
            output.WriteLine($"Measured baseline: {temperature:F1} C");
            int target = Math.Clamp((int)Math.Floor(temperature) - 3, -20, 25);
            camera.TemperatureSetPoint = target;
            camera.CoolerOn = true;
            camera.EnableSubSample = true;
            camera.SubSampleWidth = camera.SubSampleHeight = 64;
            camera.StartExposure(Sequence(600));
            var ready = camera.WaitUntilExposureIsReady(deadline.Token);
            await Until(() => PhaseIsExposing());
            await Task.Delay(3000, deadline.Token);
            var clock = Stopwatch.StartNew();
            camera.TemperatureSetPoint = target - 3;
            output.WriteLine($"Colder target acknowledged in {clock.Elapsed.TotalMilliseconds:F0} ms");
            Assert.True(clock.Elapsed < TimeSpan.FromSeconds(10));
            Assert.Equal(target - 3, (await Diagnostics()).GetProperty("values").GetProperty("16").GetInt32());
            double minimum = temperature;
            for (int i = 0; i < (direct ? 45 : 90); i++) {
                await Task.Delay(1000, deadline.Token);
                Assert.False(ready.IsCompleted);
                Assert.True(double.IsFinite(camera.Temperature) && double.IsFinite(camera.CoolerPower));
                minimum = Math.Min(minimum, camera.Temperature);
                if (i % 5 == 0) output.WriteLine($"Cooling: {camera.Temperature:F1} C, {camera.CoolerPower:F0}%, target {target - 3}");
            }
            Assert.True(minimum < temperature - .5, $"No measured cooling: {temperature} -> {minimum}");
            double beforeAbort = camera.CoolerPower;
            clock.Restart();
            camera.AbortExposure();
            await Assert.ThrowsAsync<IOException>(() => ready.WaitAsync(TimeSpan.FromSeconds(10)));
            var stopped = await Diagnostics();
            Assert.Equal(pid, stopped.GetProperty("processId").GetInt32());
            Assert.True(stopped.GetProperty("controlConnectionAvailable").GetBoolean());
            Assert.Equal(1, stopped.GetProperty("values").GetProperty("17").GetInt32());
            output.WriteLine($"Abort drained in {clock.Elapsed.TotalMilliseconds:F0} ms; power before {beforeAbort}, after {stopped.GetProperty("values").GetProperty("15")}");
            camera.StartExposure(Sequence(.1));
            await camera.WaitUntilExposureIsReady(deadline.Token);
            Assert.NotNull(await camera.DownloadExposure(deadline.Token));
            Assert.Equal("ZWO ASI585MM Pro", delivered!.Camera.Name);
            Assert.Equal(pid, (await Diagnostics()).GetProperty("processId").GetInt32());

            camera.StartExposure(Sequence(600));
            using var caller = CancellationTokenSource.CreateLinkedTokenSource(deadline.Token);
            ready = camera.WaitUntilExposureIsReady(caller.Token);
            await Until(() => PhaseIsExposing());
            camera.TemperatureSetPoint = Math.Clamp((int)Math.Ceiling(temperature) + 2, -20, 30);
            if (direct) await Until(async () => (await Diagnostics()).GetProperty("values").GetProperty("15").GetInt32() == 0);
            camera.CoolerOn = false;
            Assert.Equal(0, (await Diagnostics()).GetProperty("values").GetProperty("17").GetInt32());
            caller.Cancel();
            await Assert.ThrowsAnyAsync<OperationCanceledException>(() => ready.WaitAsync(TimeSpan.FromSeconds(10)));
            Assert.Equal(pid, (await Diagnostics()).GetProperty("processId").GetInt32());
            camera.StartExposure(Sequence(.1));
            await camera.WaitUntilExposureIsReady(deadline.Token);
            Assert.NotNull(await camera.DownloadExposure(deadline.Token));
            Assert.False(camera.CoolerOn);
            Assert.Equal(0, (await Diagnostics()).GetProperty("retry").GetProperty("recaptures").GetInt32());
        }
        finally {
            if (camera.Connected) {
                camera.AbortExposure();
                try { camera.CoolerOn = false; } finally { camera.Disconnect(); }
            }
        }
        async Task<JsonElement> Diagnostics() => (await host!.CallAsync("diagnostics", null, TimeSpan.FromSeconds(15), deadline.Token)).Result;
        async Task<bool> PhaseIsExposing() => (await Diagnostics()).GetProperty("phase").GetString() == "Exposing";
        async Task Until(Func<Task<bool>> condition) {
            var wait = Stopwatch.StartNew();
            while (!await condition()) {
                Assert.True(wait.Elapsed < TimeSpan.FromSeconds(15));
                await Task.Delay(25, deadline.Token);
            }
        }
    }

    [Asi6200HardwareFact]
    public async Task Asi6200DirectEarlyAndGatedAbortRestartThroughNina()
    {
        const string serial = "1b07450d17010900";
        const string model = "ZWO ASI6200MM Pro";
        string root = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../"));
        string settingsFolder = Path.Combine(Path.GetTempPath(), "regain-asi6200-acceptance-" + Guid.NewGuid().ToString("N"));
        string settingsPath = Path.Combine(settingsFolder, "camera.json");
        var store = new CameraSelectionStore(settingsPath);
        // Pin selection before opening USB: never test another same-model camera.
        store.Save(new(new(model, 9576, 6388, false, 0, 3.76, 16, true, false, [1, 2, 3, 4]), serial, UseDirectDriver: true));
        HostClient? host = null;
        ushort[]? deliveredPixels = null;
        ImageMetaData? deliveredMetadata = null;
        var images = new Mock<IExposureDataFactory>();
        images.Setup(f => f.CreateImageArrayExposureData(It.IsAny<ushort[]>(), It.IsAny<int>(), It.IsAny<int>(), It.IsAny<int>(), It.IsAny<bool>(), It.IsAny<ImageMetaData>()))
            .Returns((ushort[] pixels, int width, int height, int bits, bool color, ImageMetaData metadata) => {
                deliveredPixels = pixels.ToArray();
                deliveredMetadata = metadata;
                output.WriteLine($"Image: {width}x{height}, mean {pixels.Average(v => (double)v):F2}, model {metadata.Camera.Name}");
                return new ImageArrayExposureData(pixels, width, height, bits, color, metadata, Mock.Of<IImageDataFactory>());
            });
        var camera = new ResilientCamera(images.Object, store,
            () => host = new HostClient(Path.Combine(root, "target/debug/regain-alpaca.exe"), Path.Combine(root, "vendor/zwo/ASICamera2.dll"),
                direct: true, supervised: true, log: output.WriteLine),
            new() { MaxRetries = 0, ReconnectDelaySeconds = 1, CoolingTimeoutSeconds = 60 });
        using var deadline = new CancellationTokenSource(TimeSpan.FromMinutes(3));
        try {
            Assert.True(await camera.Connect(deadline.Token));
            var initial = await Diagnostics();
            Assert.Equal(serial, initial.GetProperty("serial").GetString());
            Assert.Equal("direct", initial.GetProperty("backend").GetString());
            int supervisorPid = host!.ProcessId;
            int workerPid = initial.GetProperty("processId").GetInt32();
            output.WriteLine($"Hardware: {camera.Name}, serial {serial}; supervisor {supervisorPid}, raw worker {workerPid}");
            await Task.Delay(2500, deadline.Token);
            double baselineTemperature = (await host.CallAsync("get", new { control = 8 }, TimeSpan.FromSeconds(15), deadline.Token)).Result.GetInt64() / 10.0;
            int target = Math.Clamp((int)Math.Floor(baselineTemperature) - 3, -20, 25);
            camera.TemperatureSetPoint = target;
            camera.CoolerOn = true;
            Assert.True(camera.HasDewHeater);
            camera.DewHeaterOn = true;
            camera.EnableSubSample = true;
            camera.SubSampleWidth = camera.SubSampleHeight = 64;
            var previous = await FreshImage();

            foreach (bool earlyCallerCancellation in new[] { true, false }) {
                using var caller = CancellationTokenSource.CreateLinkedTokenSource(deadline.Token);
                camera.StartExposure(Sequence(15));
                var ready = camera.WaitUntilExposureIsReady(earlyCallerCancellation ? caller.Token : deadline.Token);
                var clock = Stopwatch.StartNew();
                // Reproduce the incident's caller cancellation shortly after
                // StartExposure, then exercise all long-exposure clock gates.
                await Task.Delay(earlyCallerCancellation ? 170 : 2000, deadline.Token);
                Assert.False(ready.IsCompleted);
                clock.Restart();
                if (earlyCallerCancellation) {
                    caller.Cancel();
                    await Assert.ThrowsAnyAsync<OperationCanceledException>(() => ready.WaitAsync(TimeSpan.FromSeconds(15)));
                } else {
                    camera.AbortExposure();
                    await Assert.ThrowsAsync<IOException>(() => ready.WaitAsync(TimeSpan.FromSeconds(15)));
                }
                var stopped = await Diagnostics();
                Assert.Equal("Aborted", stopped.GetProperty("phase").GetString());
                Assert.True(stopped.GetProperty("controlConnectionAvailable").GetBoolean());
                Assert.Equal(supervisorPid, host.ProcessId);
                Assert.Equal(workerPid, stopped.GetProperty("processId").GetInt32());
                Assert.Equal(target, stopped.GetProperty("values").GetProperty("16").GetInt32());
                Assert.Equal(1, stopped.GetProperty("values").GetProperty("17").GetInt32());
                Assert.Equal(1, stopped.GetProperty("values").GetProperty("21").GetInt32());
                Assert.True(double.IsFinite(camera.Temperature) && double.IsFinite(camera.CoolerPower));
                output.WriteLine($"{(earlyCallerCancellation ? "170 ms caller cancellation" : "2 s AbortExposure")}: drained after {clock.Elapsed.TotalMilliseconds:F0} ms; cooler/heater retained");
                var pixels = await FreshImage();
                // A valid envelope alone can conceal stale sensor rows. Real
                // sensor noise must differ from the previous delivered image.
                Assert.False(previous.SequenceEqual(pixels), "Following exposure returned identical stale pixels");
                previous = pixels;
                var restarted = await Diagnostics();
                Assert.Equal(workerPid, restarted.GetProperty("processId").GetInt32());
                Assert.Equal(supervisorPid, host.ProcessId);
                Assert.Equal(0, restarted.GetProperty("retry").GetProperty("recaptures").GetInt32());
                Assert.Equal(0, restarted.GetProperty("retry").GetProperty("usbReads").GetInt32());
            }
        }
        finally {
            try {
                if (camera.Connected) {
                    try {
                        camera.AbortExposure();
                        if (camera.HasDewHeater) camera.DewHeaterOn = false;
                    }
                    finally { camera.CoolerOn = false; }
                }
            }
            finally {
                camera.Disconnect();
                if (File.Exists(settingsPath)) File.Delete(settingsPath);
                if (Directory.Exists(settingsFolder)) Directory.Delete(settingsFolder);
            }
        }
        async Task<JsonElement> Diagnostics() => (await host!.CallAsync("diagnostics", null, TimeSpan.FromSeconds(15), deadline.Token)).Result;
        async Task<ushort[]> FreshImage() {
            deliveredPixels = null;
            // A short exposure bypasses the long-exposure handshake that failed
            // in the incident. Restart through that handshake every time.
            camera.StartExposure(Sequence(2));
            await camera.WaitUntilExposureIsReady(deadline.Token).WaitAsync(TimeSpan.FromSeconds(30));
            Assert.NotNull(await camera.DownloadExposure(deadline.Token));
            Assert.Equal(model, deliveredMetadata!.Camera.Name);
            Assert.NotNull(deliveredPixels);
            Assert.Equal(64 * 64, deliveredPixels.Length);
            Assert.Contains(deliveredPixels, pixel => pixel != 0);
            return deliveredPixels;
        }
    }
    private static CaptureSequence Sequence(double seconds) => new() { ExposureTime = seconds, Binning = new BinningMode(1, 1) };
}
