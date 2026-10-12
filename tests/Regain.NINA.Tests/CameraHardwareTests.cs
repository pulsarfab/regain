using System.Diagnostics;
using System.Collections.Concurrent;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
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

public sealed class Asi6200HardwareTheoryAttribute : TheoryAttribute
{
    public Asi6200HardwareTheoryAttribute() {
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
        string workers = Path.GetFullPath(Environment.GetEnvironmentVariable("REGAIN_TEST_HARDWARE_WORKERS")
            ?? Environment.GetEnvironmentVariable("REGAIN_TEST_WORKERS")
            ?? Path.Combine(root, "target/release"));
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
                deliveredPixels = pixels;
                deliveredMetadata = metadata;
                output.WriteLine($"Image: {width}x{height}, mean {pixels.Average(v => (double)v):F2}, model {metadata.Camera.Name}");
                return new ImageArrayExposureData(pixels, width, height, bits, color, metadata, Mock.Of<IImageDataFactory>());
            });
        var camera = new ResilientCamera(images.Object, store,
            () => host = new HostClient(Path.Combine(workers, "regain-alpaca.exe"), Path.Combine(root, "vendor/zwo/ASICamera2.dll"),
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
            camera.EnableSubSample = false;
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
            camera.StartExposure(Sequence(5));
            await camera.WaitUntilExposureIsReady(deadline.Token).WaitAsync(TimeSpan.FromSeconds(30));
            Assert.NotNull(await camera.DownloadExposure(deadline.Token));
            Assert.Equal(model, deliveredMetadata!.Camera.Name);
            Assert.NotNull(deliveredPixels);
            Assert.Equal(9576 * 6388, deliveredPixels.Length);
            Assert.Contains(deliveredPixels, pixel => pixel != 0);
            images.Invocations.Clear();
            return deliveredPixels;
        }
    }
    [Asi6200HardwareTheory]
    [InlineData(true)]
    [InlineData(false)]
    public async Task Asi6200FullFrameRepeatThroughNina(bool direct)
    {
        const string serial = "1b07450d17010900";
        const string model = "ZWO ASI6200MM Pro";
        const int width = 9576, height = 6388;
        string root = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../"));
        string workers = Path.GetFullPath(Environment.GetEnvironmentVariable("REGAIN_TEST_HARDWARE_WORKERS")
            ?? Environment.GetEnvironmentVariable("REGAIN_TEST_WORKERS") ?? Path.Combine(root, "target/release"));
        string supervisor = Path.Combine(workers, "regain-alpaca.exe");
        Assert.True(File.Exists(supervisor), $"Build optimized workers or set REGAIN_TEST_HARDWARE_WORKERS: {supervisor}");
        Assert.True(File.Exists(Path.Combine(workers, "regain-device.exe")), $"Missing raw worker in {workers}");
        string settingsFolder = Path.Combine(Path.GetTempPath(), "regain-asi6200-repeat-" + Guid.NewGuid().ToString("N"));
        string settingsPath = Path.Combine(settingsFolder, "camera.json");
        var store = new CameraSelectionStore(settingsPath);
        // Pin identity before USB is opened. No fallback factory is supplied:
        // all three full frames must use the selected backend and connection.
        store.Save(new(new(model, width, height, false, 0, 3.76, 16, true, false, [1, 2, 3, 4]), serial, UseDirectDriver: direct));
        HostClient? host = null;
        int hostStarts = 0, deliveries = 0;
        var records = new ConcurrentQueue<string>();
        void Log(string line) {
            records.Enqueue(line);
            // Exercise the same structured parser and INFO routing as the
            // installed plugin, rather than accepting raw stderr alone.
            CameraLog.Forward(CameraLog.Parse(direct ? "direct" : "sdk", line), output.WriteLine, output.WriteLine, output.WriteLine);
        }
        HardwareFrameSummary? delivered = null, previous = null;
        var images = new Mock<IExposureDataFactory>();
        images.Setup(f => f.CreateImageArrayExposureData(It.IsAny<ushort[]>(), It.IsAny<int>(), It.IsAny<int>(), It.IsAny<int>(), It.IsAny<bool>(), It.IsAny<ImageMetaData>()))
            .Returns((ushort[] pixels, int w, int h, int bits, bool color, ImageMetaData metadata) => {
                Assert.Equal(width, w);
                Assert.Equal(height, h);
                Assert.Equal(width * height, pixels.Length);
                Assert.Equal(16, bits);
                Assert.False(color);
                Assert.Equal(model, metadata.Camera.Name);
                Assert.Equal(100, metadata.Camera.Gain);
                Assert.Equal(50, metadata.Camera.Offset);
                Assert.Equal(1, metadata.Camera.BinX);
                Assert.Equal(1, metadata.Camera.BinY);
                Assert.Equal(5, metadata.Image.ExposureTime);
                Assert.NotEqual(DateTime.MinValue, metadata.Image.ExposureStart);
                delivered = SummarizeHardwareFrame(pixels, w, h);
                deliveries++;
                return new ImageArrayExposureData(pixels, w, h, bits, color, metadata, Mock.Of<IImageDataFactory>());
            });
        var camera = new ResilientCamera(images.Object, store,
            () => {
                hostStarts++;
                return host = new HostClient(supervisor, Path.Combine(root, "vendor/zwo/ASICamera2.dll"),
                    direct: direct, supervised: true, log: Log);
            },
            new() {
                MaxRetries = 0, DirectReadRetries = 0, ReadyFrameDownloadRetries = 0,
                UsbResetAfterFailures = 0, ReconnectDelaySeconds = 1,
                ExposureGraceSeconds = 20, DownloadTimeoutSeconds = 20, CoolingTimeoutSeconds = 30
            });
        using var deadline = new CancellationTokenSource(TimeSpan.FromMinutes(2));
        try {
            Assert.True(await camera.Connect(deadline.Token));
            var initial = await Diagnostics();
            Assert.Equal(serial, initial.GetProperty("serial").GetString());
            Assert.Equal(direct ? "direct" : "sdk", initial.GetProperty("backend").GetString());
            Assert.False(initial.GetProperty("sdkFallback").GetBoolean());
            int supervisorPid = host!.ProcessId;
            int workerPid = initial.GetProperty("processId").GetInt32();
            output.WriteLine($"Full-frame NINA repeat: {(direct ? "direct" : "sdk")}, {model}, serial {serial}, workers {workers}; supervisor {supervisorPid}, raw worker {workerPid}");
            await Task.Delay(2500, deadline.Token);
            double baseline = (await host.CallAsync("get", new { control = 8 }, TimeSpan.FromSeconds(10), deadline.Token)).Result.GetInt64() / 10.0;
            int target = Math.Clamp((int)Math.Ceiling(baseline) - 2, -20, 25);
            camera.TemperatureSetPoint = target;
            camera.CoolerOn = true;
            Assert.True(camera.HasDewHeater);
            camera.DewHeaterOn = true;
            camera.EnableSubSample = false;
            output.WriteLine($"Thermal baseline {baseline:F1} C, mild target {target} C, cooler and heater enabled");

            for (int frame = 1; frame <= 3; frame++) {
                delivered = null;
                var clock = Stopwatch.StartNew();
                camera.StartExposure(new CaptureSequence {
                    ExposureTime = 5, Gain = 100, Offset = 50, Binning = new BinningMode(1, 1)
                });
                double startMs = clock.Elapsed.TotalMilliseconds;
                await camera.WaitUntilExposureIsReady(deadline.Token).WaitAsync(TimeSpan.FromSeconds(40), deadline.Token);
                double readyMs = clock.Elapsed.TotalMilliseconds;
                var download = Stopwatch.StartNew();
                var image = Assert.IsType<ImageArrayExposureData>(await camera.DownloadExposure(deadline.Token)
                    .WaitAsync(TimeSpan.FromSeconds(20), deadline.Token));
                double downloadMs = download.Elapsed.TotalMilliseconds;
                Assert.Equal(width, image.Width);
                Assert.Equal(height, image.Height);
                Assert.Equal(16, image.BitDepth);
                Assert.False(image.IsBayered);
                Assert.Equal(frame, deliveries);
                Assert.NotNull(delivered);
                Assert.True(delivered.Mean > 0, "The full frame contains no nonzero pixels");
                if (previous is not null) {
                    Assert.NotEqual(previous.Hash, delivered.Hash);
                    int changedRows = delivered.RowHashes.Zip(previous.RowHashes, (a, b) => a != b).Count(changed => changed);
                    double rowDeltaRms = Math.Sqrt(delivered.RowMeans.Zip(previous.RowMeans, (a, b) => (a - b) * (a - b)).Average());
                    output.WriteLine($"Frame {frame} vs {frame - 1}, offset50 pair: {changedRows}/{height} rows changed; row-mean delta RMS {rowDeltaRms:F3} ADU");
                    Assert.Equal(height, changedRows);
                }
                double evenMean = delivered.RowMeans.Where((_, row) => row % 2 == 0).Average();
                double oddMean = delivered.RowMeans.Where((_, row) => row % 2 != 0).Average();
                double maxRowStep = delivered.RowMeans.Skip(1).Zip(delivered.RowMeans, (a, b) => Math.Abs(a - b)).Max();
                string bands = string.Join(", ", Enumerable.Range(0, 8).Select(band =>
                    delivered.RowMeans.Skip(band * height / 8).Take((band + 1) * height / 8 - band * height / 8).Average().ToString("F2")));
                output.WriteLine($"Frame {frame}: StartExposure {startMs:F1} ms; ready {readyMs:F1} ms from start; DownloadExposure {downloadMs:F1} ms including factory/hash/statistics; total {clock.Elapsed.TotalMilliseconds:F1} ms; {width}x{height} RAW16, {width * height} pixels; SHA256 {delivered.Hash}");
                output.WriteLine($"Frame {frame}: mean {delivered.Mean:F2}, row means {delivered.RowMeans.Min():F2}..{delivered.RowMeans.Max():F2}, even-minus-odd {evenMean - oddMean:F3}, largest adjacent row step {maxRowStep:F3}; eight row-band means [{bands}]");
                previous = delivered; // Retain small summaries, never a second full-frame pixel copy.
                images.Invocations.Clear(); // Moq otherwise retains every 122 MB array argument.
                var state = await Diagnostics();
                Assert.Equal(1, hostStarts);
                Assert.Equal(supervisorPid, host.ProcessId);
                Assert.Equal(workerPid, state.GetProperty("processId").GetInt32());
                Assert.Equal(direct ? "direct" : "sdk", state.GetProperty("backend").GetString());
                Assert.False(state.GetProperty("sdkFallback").GetBoolean());
                Assert.True(state.GetProperty("controlConnectionAvailable").GetBoolean());
                Assert.Equal(0, state.GetProperty("retry").GetProperty("recaptures").GetInt32());
                Assert.Equal(0, state.GetProperty("retry").GetProperty("usbReads").GetInt32());
                Assert.Equal(0, state.GetProperty("retry").GetProperty("downloads").GetInt32());
                Assert.Equal(target, state.GetProperty("values").GetProperty("16").GetInt32());
                Assert.Equal(1, state.GetProperty("values").GetProperty("17").GetInt32());
                Assert.Equal(1, state.GetProperty("values").GetProperty("21").GetInt32());
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
                images.Invocations.Clear();
                if (File.Exists(settingsPath)) File.Delete(settingsPath);
                if (Directory.Exists(settingsFolder)) Directory.Delete(settingsFolder);
            }
        }
        var structured = records.Where(line => line.StartsWith("REGAIN_DIAGNOSTIC ", StringComparison.Ordinal))
            .Select(line => {
                using var document = JsonDocument.Parse(line["REGAIN_DIAGNOSTIC ".Length..]);
                return document.RootElement.Clone();
            }).ToArray();
        Assert.Contains(structured, record => record.GetProperty("event").GetString() == "ipc.command.complete"
            && record.GetProperty("method").GetString() == "download"
            && record.GetProperty("details").GetProperty("binaryBytes").GetInt32() == width * height * 2);
        Assert.DoesNotContain(structured, record => record.GetProperty("event").GetString() == "usb.log_dropped");
        Assert.Contains(structured, record => record.GetProperty("event").GetString() == "usb.submit"
            && record.GetProperty("details").TryGetProperty("requestedBytes", out var requested) && requested.GetInt64() > 0);
        Assert.Contains(structured, record => record.GetProperty("event").GetString() == "usb.complete"
            && record.GetProperty("details").GetProperty("elapsedUs").GetDouble() >= 0);
        if (!direct) {
            Assert.Contains(structured, record => record.GetProperty("event").GetString() == "sdk.usb.instrumentation"
                && record.GetProperty("details").GetProperty("available").GetBoolean());
            Assert.Contains(structured, record => record.GetProperty("event").GetString() == "sdk.api.complete"
                && record.GetProperty("details").GetProperty("api").GetString() == "ASIGetDataAfterExp");
        }
        async Task<JsonElement> Diagnostics() => (await host!.CallAsync("diagnostics", null, TimeSpan.FromSeconds(10), deadline.Token)).Result;
    }

    private sealed record HardwareFrameSummary(string Hash, double Mean, double[] RowMeans, ulong[] RowHashes);

    private static HardwareFrameSummary SummarizeHardwareFrame(ushort[] pixels, int width, int height)
    {
        string hash = Convert.ToHexString(SHA256.HashData(MemoryMarshal.AsBytes(pixels.AsSpan()))).ToLowerInvariant();
        var means = new double[height];
        var rowHashes = new ulong[height];
        long total = 0;
        for (int row = 0; row < height; row++) {
            long sum = 0;
            ulong rowHash = 14695981039346656037;
            for (int column = 0; column < width; column++) {
                ushort value = pixels[row * width + column];
                sum += value;
                rowHash = unchecked((rowHash ^ value) * 1099511628211);
            }
            means[row] = (double)sum / width;
            rowHashes[row] = rowHash;
            total += sum;
        }
        return new(hash, (double)total / pixels.Length, means, rowHashes);
    }

    private static CaptureSequence Sequence(double seconds) => new() { ExposureTime = seconds, Binning = new BinningMode(1, 1) };
}
