using System.Text.Json;
using Moq;
using NINA.Core.Model.Equipment;
using NINA.Equipment.Model;
using NINA.Image.ImageData;
using NINA.Image.Interfaces;
using Regain.Core;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class UsbPeripheralCameraTests
{
    private sealed class Wheel : IUsbPeripheral
    {
        private PeripheralState current = new(true, "selected-wheel", "filter=3");
        internal readonly TaskCompletionSource Entered = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal readonly TaskCompletionSource Release = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal bool ChangedFilter;
        internal int Connections, Disposals;
        public string Kind => "filter wheel";
        public event Action? StateChanged;
        public PeripheralState Snapshot() => Volatile.Read(ref current);
        internal void Report(bool connected, string state)
        {
            Volatile.Write(ref current, new(connected, "selected-wheel", state)); StateChanged?.Invoke();
        }
        public async Task<bool> ReconnectAsync(CancellationToken token)
        {
            Interlocked.Increment(ref Connections); Entered.TrySetResult();
            await Release.Task.WaitAsync(token);
            Report(true, ChangedFilter ? "filter=0" : "filter=3"); return true;
        }
        public void Dispose() => Interlocked.Increment(ref Disposals);
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task NinaSupervisorWaitsForSameFilterBeforeReplacementAndNeverReturnsChangedFilterImage(bool changedFilter)
    {
        // This is a simulated supervisor/NINA lifecycle test; neither USB nor
        // an installed NINA session is opened. A small fixture image is sufficient.
        string root = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../"));
        string workers = Environment.GetEnvironmentVariable("REGAIN_TEST_WORKERS") ?? Path.Combine(root, "target", "debug");
        HostClient? host = null;
        var wheel = new Wheel { ChangedFilter = changedFilter };
        var images = new Mock<IExposureDataFactory>();
        images.Setup(factory => factory.CreateImageArrayExposureData(It.IsAny<ushort[]>(), It.IsAny<int>(), It.IsAny<int>(),
            It.IsAny<int>(), It.IsAny<bool>(), It.IsAny<ImageMetaData>())).Returns((ushort[] pixels, int width, int height, int bits, bool bayered, ImageMetaData metadata) =>
                new ImageArrayExposureData(pixels, width, height, bits, bayered, metadata, Mock.Of<IImageDataFactory>()));
        var camera = new ResilientCamera(new("ZWO ASI676MC", 3552, 3552, true, 0, 2, 12, false, false, [1]), images.Object,
            () => host = new HostClient(Path.Combine(workers, "regain-alpaca.exe"), "unused", simulate: true, direct: true, supervised: true),
            new RecoveryOptions { MaxRetries = 1, UsbResetAfterFailures = 1, ReconnectDelaySeconds = .03 },
            peripheralRecoveryFactory: () => new UsbPeripheralRecovery([wheel], new() { DisconnectWindowSeconds = 1 }, _ => { }));
        Task? ready = null;
        Task<IExposureData>? image = null;
        try {
            Assert.True(await camera.Connect(CancellationToken.None));
            // Subscribe after CameraSession has subscribed, so the real reset
            // anchor snapshots the connected wheel before the hub disconnect.
            host!.UsbRecovery += recovery => {
                if (recovery.Phase == CameraUsbRecoveryPhase.Resetting) wheel.Report(false, "filter=3");
            };
            await host.CallAsync("simulate-read-failures", new { count = 3 }, TimeSpan.FromSeconds(15), CancellationToken.None);
            camera.EnableSubSample = true; camera.SubSampleWidth = camera.SubSampleHeight = 64;
            var request = new CaptureSequence { ExposureTime = .3, Binning = new BinningMode(1, 1), Gain = 100, Offset = 10 };
            camera.StartExposure(request);
            ready = camera.WaitUntilExposureIsReady(CancellationToken.None);
            image = camera.DownloadExposure(CancellationToken.None);
            await wheel.Entered.Task.WaitAsync(TimeSpan.FromSeconds(20));
            var waiting = (await host.CallAsync("status", null, TimeSpan.FromSeconds(5), CancellationToken.None)).Result;
            Assert.Equal("Waiting for NINA peripherals", waiting.GetProperty("phase").GetString());
            Assert.True(waiting.GetProperty("usbPeripheralRecoveryId").GetUInt64() > 0);
            Assert.False(ready.IsCompleted); Assert.False(image.IsCompleted); Assert.True(camera.Connected);
            images.Verify(factory => factory.CreateImageArrayExposureData(It.IsAny<ushort[]>(), It.IsAny<int>(), It.IsAny<int>(),
                It.IsAny<int>(), It.IsAny<bool>(), It.IsAny<ImageMetaData>()), Times.Never);
            wheel.Release.TrySetResult();
            if (changedFilter) {
                var failure = await Assert.ThrowsAsync<IOException>(() => ready.WaitAsync(TimeSpan.FromSeconds(15)));
                Assert.Contains("optical state", failure.Message);
                await Assert.ThrowsAsync<IOException>(() => image.WaitAsync(TimeSpan.FromSeconds(5)));
                var failed = (await host.CallAsync("status", null, TimeSpan.FromSeconds(5), CancellationToken.None)).Result;
                Assert.NotEqual(2, failed.GetProperty("state").GetInt32());
                Assert.Equal(JsonValueKind.Null, failed.GetProperty("usbPeripheralRecoveryId").ValueKind);
                // A later request must still be blocked until an explicit camera reconnect.
                camera.StartExposure(request);
                var blocked = await Assert.ThrowsAnyAsync<IOException>(() => camera.WaitUntilExposureIsReady(CancellationToken.None));
                Assert.Contains("peripheral", blocked.Message, StringComparison.OrdinalIgnoreCase);
                images.Verify(factory => factory.CreateImageArrayExposureData(It.IsAny<ushort[]>(), It.IsAny<int>(), It.IsAny<int>(),
                    It.IsAny<int>(), It.IsAny<bool>(), It.IsAny<ImageMetaData>()), Times.Never);
            } else {
                await ready.WaitAsync(TimeSpan.FromSeconds(15));
                var delivered = Assert.IsType<ImageArrayExposureData>(await image.WaitAsync(TimeSpan.FromSeconds(5)));
                Assert.Equal(64, delivered.Width); Assert.Equal(64, delivered.Height);
                Assert.Equal(100, delivered.MetaData.Camera.Gain);
                Assert.True(camera.Connected);
                images.Verify(factory => factory.CreateImageArrayExposureData(It.IsAny<ushort[]>(), It.IsAny<int>(), It.IsAny<int>(),
                    It.IsAny<int>(), It.IsAny<bool>(), It.IsAny<ImageMetaData>()), Times.Once);
            }
            Assert.Equal(1, wheel.Connections);
        } finally {
            wheel.Release.TrySetResult(); camera.Disconnect();
            if (ready is not null) try { await ready; } catch { }
            if (image is not null) try { await image; } catch { }
        }
        Assert.Equal(1, wheel.Disposals);
    }
}
