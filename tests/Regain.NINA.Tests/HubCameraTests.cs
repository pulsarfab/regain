using System.ComponentModel.Composition;
using Moq;
using NINA.Core.Model.Equipment;
using NINA.Core.Enum;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.ViewModel;
using NINA.Equipment.Model;
using NINA.Image.Interfaces;
using NINA.Image.ImageData;
using NINA.Image.ImageAnalysis;
using NINA.Profile.Interfaces;
using Regain.Hub;
using Regain.TestFixtures;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class HubCameraTests
{
    [Fact]
    public void ProviderExportsNativeCameraAndEnumerationDoesNotConnect()
    {
        Assert.Contains(typeof(HubCameraProvider).GetCustomAttributes(typeof(ExportAttribute), false).Cast<ExportAttribute>(),
            attribute => attribute.ContractType == typeof(IEquipmentProvider));
        using var camera = new HubCameraDevice(null, Mock.Of<IImageDataFactory>(), executable: "absent-hub.exe");
        Assert.False(camera.Connected);
        Assert.Contains("Configure.camera", camera.Id);
        Assert.False(camera.HasDewHeater);
        Assert.False(camera.CanShowLiveView);
        Assert.False(camera.CanSetUSBLimit);
    }
    [Theory, InlineData(false, false, false, false), InlineData(true, false, false, true), InlineData(false, true, false, false), InlineData(false, true, true, true)]
    public async Task NativeCameraSharesExactCaptureAndRestoresProfileTimeout(bool direct, bool standard, bool nested, bool signed)
    {
        var root = new DirectoryInfo(AppContext.BaseDirectory);
        while (root is not null && !File.Exists(Path.Combine(root.FullName, "Cargo.toml"))) root = root.Parent;
        var workers = Environment.GetEnvironmentVariable("REGAIN_TEST_WORKERS") ?? Path.Combine(root!.FullName, "target", "debug");
        var executable = Path.Combine(workers, "regain-alpaca.exe");
        await HubCameraHostFixture.Run(executable, direct, standard, nested, async (first, second) => {
            var settings = new Mock<ICameraSettings>();
            settings.SetupProperty(value => value.Timeout, 1);
            settings.SetupGet(value => value.ASCOMCreate32BitData).Returns(signed);
            var profiles = new Mock<IProfileService>();
            profiles.Setup(value => value.ActiveProfile.CameraSettings).Returns(settings.Object);
            var factory = new Mock<IImageDataFactory>(MockBehavior.Strict);
            factory.Setup(value => value.CreateBaseImageData(It.IsAny<IImageArray>(), It.IsAny<int>(), It.IsAny<int>(), It.IsAny<int>(), It.IsAny<bool>(), It.IsAny<ImageMetaData>()))
                .Returns((IImageArray pixels, int width, int height, int depth, bool bayer, ImageMetaData metadata) =>
                    new BaseImageData(pixels, width, height, depth, bayer, metadata, profiles.Object, Mock.Of<IStarDetection>(), Mock.Of<IStarAnnotator>()));
            using var owner = new HubCameraDevice(first, factory.Object, profiles.Object, executable, workers);
            using var sibling = new HubCameraDevice(second, factory.Object, executable: executable, workers: workers);
            using var limit = new CancellationTokenSource(TimeSpan.FromSeconds(45));
            await owner.Connect(limit.Token); await sibling.Connect(limit.Token);
            Assert.True(owner.Connected); Assert.True(sibling.Connected);
            Assert.Equal(16, owner.BitDepth);
            Assert.Equal(owner.CameraXSize, sibling.CameraXSize);
            var expectedBayer = owner.SensorType != SensorType.Monochrome;
            Assert.NotEmpty(owner.ReadoutModes);
            if (owner.CanSetTemperature) {
                owner.TemperatureSetPoint = -5; owner.CoolerOn = true;
                Assert.Equal(-5, sibling.TemperatureSetPoint); Assert.True(sibling.CoolerOn);
                owner.CoolerOn = false;
            }
            owner.EnableSubSample = true; owner.SubSampleX = owner.SubSampleY = 0;
            // 585 direct mode has a traced 64x64 minimum. Keep an asymmetric
            // supported rectangle; this frontend must not bypass source limits.
            owner.SubSampleWidth = 96; owner.SubSampleHeight = 64;
            try { owner.StartExposure(new CaptureSequence { ExposureTime = 0.2, Binning = new BinningMode(1, 1), Gain = -1, Offset = -1 }); }
            catch (HubException error) { throw new InvalidOperationException($"Camera fixture start: {error.Remote?.Code}: {error.Remote?.Message}", error); }
            Assert.True(settings.Object.Timeout > 1);
            var busy = await Assert.ThrowsAsync<HubException>(() => Task.Run(() => sibling.StartExposure(new CaptureSequence {
                ExposureTime = 0.01, Binning = new BinningMode(1, 1), Gain = -1, Offset = -1 }), limit.Token));
            Assert.Equal("busy", busy.Remote?.Code);
            await owner.WaitUntilExposureIsReady(limit.Token);
            var frame = Assert.IsType<HubCameraExposureData>(await owner.DownloadExposure(limit.Token));
            Assert.Equal(96, frame.Width); Assert.Equal(64, frame.Height);
            Assert.Equal(signed ? typeof(int[]) : typeof(ushort[]), frame.Pixels.GetType());
            Assert.Equal(96 * 64, frame.Pixels.Length);
            Assert.Equal(0.2, frame.MetaData.Image.ExposureTime, 5);
            Assert.Equal(1, frame.MetaData.Camera.BinX);
            Assert.Equal(1, settings.Object.Timeout);
            var imageData = await frame.ToImageData(null!, limit.Token);
            Assert.Same(frame.Pixels, signed ? imageData.Data.FlatArrayInt : imageData.Data.FlatArray);
            Assert.Same(imageData, await frame.ToImageData(null!, limit.Token));
            factory.Verify(value => value.CreateBaseImageData(It.IsAny<IImageArray>(), 96, 64, 16, expectedBayer, frame.MetaData), Times.Once);
            Assert.True(sibling.Connected);
            var firstPixel = frame.Pixels.GetValue(0);
            owner.StartExposure(new CaptureSequence { ExposureTime = 0.01, Binning = new BinningMode(1, 1), Gain = -1, Offset = -1 });
            await owner.WaitUntilExposureIsReady(limit.Token);
            settings.Object.Timeout = 12345; // A user edit wins over restoration.
            await owner.DownloadExposure(limit.Token);
            Assert.Equal(12345, settings.Object.Timeout);
            Assert.Equal(firstPixel, frame.Pixels.GetValue(0));
            settings.Object.Timeout = 1;
            owner.StartExposure(new CaptureSequence { ExposureTime = 0.2, Binning = new BinningMode(1, 1), Gain = -1, Offset = -1 });
            using (var cancel = new CancellationTokenSource()) {
                cancel.Cancel();
                await Assert.ThrowsAnyAsync<OperationCanceledException>(() => owner.WaitUntilExposureIsReady(cancel.Token));
                Assert.Equal(1, settings.Object.Timeout);
            }
            await owner.WaitUntilExposureIsReady(limit.Token);
            var uncancelled = Assert.IsType<HubCameraExposureData>(await owner.DownloadExposure(limit.Token));
            Assert.Equal(0.2, uncancelled.MetaData.Image.ExposureTime, 5);
            Assert.True(sibling.Connected);
            sibling.StartExposure(new CaptureSequence { ExposureTime = 0.01, Binning = new BinningMode(1, 1), Gain = -1, Offset = -1 });
            await sibling.WaitUntilExposureIsReady(limit.Token);
            var replaced = await Assert.ThrowsAsync<HubException>(() => owner.DownloadExposure(limit.Token));
            Assert.Equal("unavailable", replaced.Remote?.Code);
            Assert.True(owner.Connected); Assert.Equal(1, settings.Object.Timeout);
            if (standard) {
                owner.StartExposure(new CaptureSequence { ExposureTime = 5, Binning = new BinningMode(1, 1), Gain = -1, Offset = -1 });
                var wrongOwner = Assert.Throws<HubException>(() => sibling.AbortExposure());
                Assert.Equal("busy", wrongOwner.Remote?.Code);
                owner.AbortExposure();
                await Assert.ThrowsAsync<IOException>(() => owner.WaitUntilExposureIsReady(limit.Token));
                Assert.Equal(1, settings.Object.Timeout);
                Assert.Equal(CameraStates.Error, owner.CameraState);
                owner.StartExposure(new CaptureSequence { ExposureTime = 5, Binning = new BinningMode(1, 1), Gain = -1, Offset = -1 });
                owner.StopExposure();
                await owner.WaitUntilExposureIsReady(limit.Token);
                var stopped = Assert.IsType<HubCameraExposureData>(await owner.DownloadExposure(limit.Token));
                Assert.InRange(stopped.MetaData.Image.ExposureTime, 0, 5);
                Assert.Equal(1, settings.Object.Timeout); Assert.True(sibling.Connected);
            }
            owner.StartExposure(new CaptureSequence { ExposureTime = 2, Binning = new BinningMode(1, 1), Gain = -1, Offset = -1 });
            Assert.True(settings.Object.Timeout > 1);
            var retiring = owner.WaitUntilExposureIsReady(limit.Token);
            Assert.False(retiring.IsCompleted);
            owner.Disconnect(); Assert.Equal(1, settings.Object.Timeout); Assert.True(sibling.Connected);
            await Assert.ThrowsAnyAsync<OperationCanceledException>(() => retiring);
        });
    }
}
