using System.ComponentModel.Composition;
using Moq;
using NINA.Core.Model.Equipment;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.ViewModel;
using NINA.Equipment.Model;
using NINA.Image.ImageData;
using NINA.Image.Interfaces;
using ZwoGain.Core;
using Xunit;

namespace ZwoGain.NINA.Tests;

public class CameraTests
{
    [Fact]
    public void ExportsCameraProvider()
    {
        Assert.Contains(typeof(CameraProvider).GetCustomAttributes(typeof(ExportAttribute), false).Cast<ExportAttribute>(), a => a.ContractType == typeof(IEquipmentProvider));
        Assert.Contains(typeof(ICamera), typeof(ResilientCamera).GetInterfaces());
    }
    [Fact]
    public async Task NinaLifecycleHidesDownloadFailureAndPreservesCaptureMetadata()
    {
        string root = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../"));
        int starts = 0;
        var factory = new Mock<IExposureDataFactory>();
        factory.Setup(f => f.CreateImageArrayExposureData(It.IsAny<ushort[]>(), It.IsAny<int>(), It.IsAny<int>(), It.IsAny<int>(), It.IsAny<bool>(), It.IsAny<ImageMetaData>()))
            .Returns((ushort[] p, int w, int h, int b, bool color, ImageMetaData m) => new ImageArrayExposureData(p, w, h, b, color, m, Mock.Of<IImageDataFactory>()));
        HostClient Host()
        {
            var host = new HostClient(Path.Combine(root, "target/debug/zwogain-host.exe"), "unused", true);
            if (starts++ == 0)
                host.CallAsync("fault", new
                {
                    kind = "download"
                }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
            return host;
        }
        var camera = new ResilientCamera(new("ZWO Simulated", 960, 640, true, 0, 3.76, 16, true, false, [1, 2, 4]), factory.Object, Host,
            new()
            {
                MaxRetries = 1,
                ReconnectDelaySeconds = .05,
                CoolingStableSamples = 1,
                CoolingSampleSeconds = .01
            });
        try
        {
            Assert.True(await camera.Connect(default));
            camera.StartExposure(new CaptureSequence { ExposureTime = .01, Gain = 123, Offset = 17, Binning = new BinningMode(2, 2) });
            await camera.WaitUntilExposureIsReady(default);
            camera.Gain = 222; // Image metadata must represent the completed exposure, not the next one.
            var image = Assert.IsType<ImageArrayExposureData>(await camera.DownloadExposure(default));
            Assert.Equal(480, image.Width);
            Assert.Equal(320, image.Height);
            Assert.Equal(16, image.BitDepth);
            Assert.True(image.IsBayered);
            Assert.Equal(123, image.MetaData.Camera.Gain);
            Assert.Equal(17, image.MetaData.Camera.Offset);
            Assert.Equal(2, image.MetaData.Camera.BinX);
            Assert.Equal(.01, image.MetaData.Image.ExposureTime);
            Assert.NotEqual(DateTime.MinValue, image.MetaData.Image.ExposureStart);
            Assert.True(camera.Connected);
            Assert.Equal(2, starts);
        }
        finally { camera.Disconnect(); }
    }
}
