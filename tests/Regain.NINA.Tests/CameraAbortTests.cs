using System.Text.Json;
using Moq;
using NINA.Core.Model.Equipment;
using NINA.Equipment.Model;
using NINA.Image.ImageData;
using NINA.Image.Interfaces;
using NINA.Profile.Interfaces;
using Regain.Core;
using Xunit;

namespace Regain.NINA.Tests;

public class CameraAbortTests
{
    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task LiveCoolingAndExplicitAbortPreserveWorkerAndAllowAutofocusRestart(bool direct)
    {
        string root = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../"));
        HostClient? host = null;
        var settings = new Mock<ICameraSettings>();
        settings.SetupProperty(s => s.Timeout, 60);
        var profiles = new Mock<IProfileService>();
        profiles.Setup(p => p.ActiveProfile.CameraSettings).Returns(settings.Object);
        var images = new Mock<IExposureDataFactory>();
        images.Setup(f => f.CreateImageArrayExposureData(It.IsAny<ushort[]>(), It.IsAny<int>(), It.IsAny<int>(), It.IsAny<int>(), It.IsAny<bool>(), It.IsAny<ImageMetaData>()))
            .Returns((ushort[] p, int w, int h, int bits, bool color, ImageMetaData m) => new ImageArrayExposureData(p, w, h, bits, color, m, Mock.Of<IImageDataFactory>()));
        var descriptor = direct
            ? new CameraDescriptor("ZWO ASI585MM Pro", 3840, 2160, false, 0, 2.9, 12, true, false, [1, 2, 3, 4])
            : new CameraDescriptor("ZWO Simulated", 960, 640, true, 0, 3.76, 16, true, false, [1, 2, 4]);
        var camera = new ResilientCamera(descriptor, images.Object,
            () => host = new HostClient(Path.Combine(root, "target/debug/regain-alpaca.exe"), "unused", simulate: true, direct: direct, supervised: true),
            new() { MaxRetries = 1, ReconnectDelaySeconds = .01, CoolingStableSamples = 1, CoolingSampleSeconds = .01 }, profiles.Object);
        try {
            Assert.True(await camera.Connect(default));
            camera.TemperatureSetPoint = 25;
            camera.CoolerOn = true;
            camera.EnableSubSample = true;
            camera.SubSampleWidth = camera.SubSampleHeight = 64;
            using var oldCaller = new CancellationTokenSource();
            camera.StartExposure(Sequence(600));
            var waiting = camera.WaitUntilExposureIsReady(oldCaller.Token);
            var before = await Exposing(host!);
            int pid = before.GetProperty("processId").GetInt32();
            // Hardware write/readback is acknowledged while the 600s capture
            // still owns the worker, including a warmer target and cooler-off.
            camera.TemperatureSetPoint = 26;
            camera.CoolerOn = false;
            Assert.False(waiting.IsCompleted);
            var live = await Diagnostics(host!);
            Assert.Equal(26, live.GetProperty("values").GetProperty("16").GetInt32());
            Assert.Equal(0, live.GetProperty("values").GetProperty("17").GetInt32());
            camera.AbortExposure();
            // This must not enter NINA's OCE => readiness-timeout branch.
            var error = await Assert.ThrowsAsync<IOException>(() => waiting.WaitAsync(TimeSpan.FromSeconds(10)));
            Assert.Contains("aborted by the client", error.Message);
            Assert.Equal(60, settings.Object.Timeout);
            var stopped = await Diagnostics(host!);
            Assert.Equal(pid, stopped.GetProperty("processId").GetInt32());
            Assert.True(stopped.GetProperty("controlConnectionAvailable").GetBoolean());
            Assert.Equal(JsonValueKind.Null, stopped.GetProperty("retry").GetProperty("lastFailure").ValueKind);
            camera.StartExposure(Sequence(.1));
            oldCaller.Cancel(); // An old readiness callback cannot abort this generation.
            await camera.WaitUntilExposureIsReady(default).WaitAsync(TimeSpan.FromSeconds(10));
            Assert.NotNull(await camera.DownloadExposure(default));
            Assert.Equal(pid, (await Diagnostics(host!)).GetProperty("processId").GetInt32());
            Assert.False(camera.CoolerOn);
        }
        finally { camera.Disconnect(); }
    }

    [Fact]
    public async Task CallerCancellationStillUsesNinaCancellationContractAndDrainsAbort()
    {
        string root = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../"));
        HostClient? host = null;
        var camera = new ResilientCamera(new("ZWO Simulated", 960, 640, true, 0, 3.76, 16, true, false, [1, 2, 4]),
            Mock.Of<IExposureDataFactory>(), () => host = new HostClient(Path.Combine(root, "target/debug/regain-alpaca.exe"), "unused", simulate: true, supervised: true), new());
        try {
            await camera.Connect(default);
            camera.StartExposure(Sequence(600));
            using var caller = new CancellationTokenSource();
            var ready = camera.WaitUntilExposureIsReady(caller.Token);
            int pid = (await Exposing(host!)).GetProperty("processId").GetInt32();
            caller.Cancel();
            await Assert.ThrowsAnyAsync<OperationCanceledException>(() => ready.WaitAsync(TimeSpan.FromSeconds(10)));
            Assert.Equal(pid, (await Diagnostics(host!)).GetProperty("processId").GetInt32());
            // Readiness only returns cancellation after the previous abort drains.
            camera.StartExposure(Sequence(.01));
            await camera.WaitUntilExposureIsReady(default).WaitAsync(TimeSpan.FromSeconds(10));
        }
        finally { camera.Disconnect(); }
    }

    private static CaptureSequence Sequence(double seconds) => new() { ExposureTime = seconds, Binning = new BinningMode(1, 1) };
    private static async Task<JsonElement> Diagnostics(HostClient host) =>
        (await host.CallAsync("diagnostics", null, TimeSpan.FromSeconds(10), default)).Result;
    private static async Task<JsonElement> Exposing(HostClient host)
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(10));
        while (true) {
            var state = await Diagnostics(host);
            if (state.GetProperty("phase").GetString() == "Exposing") return state;
            await Task.Delay(10, deadline.Token);
        }
    }
}
