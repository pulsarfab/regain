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
    private static CaptureSequence Sequence(double seconds) => new() { ExposureTime = seconds, Binning = new BinningMode(1, 1) };
}
