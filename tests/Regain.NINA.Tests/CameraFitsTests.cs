using System.Text;
using Moq;
using NINA.Core.Model.Equipment;
using NINA.Equipment.Equipment.MyCamera;
using NINA.Equipment.Model;
using NINA.Equipment.Utility;
using NINA.Image.FileFormat.FITS;
using NINA.Image.ImageData;
using NINA.Image.Interfaces;
using Regain.Core;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class CameraFitsTests
{
    [Theory]
    [InlineData("ZWO ASI585MM Pro", 3840, 2160, true)]
    [InlineData("ZWO ASI2600MM Duo", 6248, 4176, true)]
    [InlineData("ZWO ASI2600MM Pro", 6248, 4176, true)]
    [InlineData("ZWO ASI220MM Mini", 1920, 1080, true)]
    [InlineData("ZWO ASI6200MM Pro", 9576, 6388, true)]
    [InlineData("ZWO Simulated", 960, 640, false)]
    public async Task FitsInstrumentPreservesConnectedModelInsteadOfSetupOrProviderLabel(string model, int width, int height, bool direct)
    {
        var images = new Mock<IExposureDataFactory>();
        images.Setup(f => f.CreateImageArrayExposureData(It.IsAny<ushort[]>(), It.IsAny<int>(), It.IsAny<int>(), It.IsAny<int>(), It.IsAny<bool>(), It.IsAny<ImageMetaData>()))
            .Returns((ushort[] pixels, int w, int h, int depth, bool color, ImageMetaData metadata) =>
                new ImageArrayExposureData(pixels, w, h, depth, color, metadata, Mock.Of<IImageDataFactory>()));
        string worker = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../target/debug/regain-device.exe"));
        var camera = new ResilientCamera(new(model, width, height, !direct, 0, 3.76, 16, model != "ZWO ASI220MM Mini", false, [1]),
            images.Object, () => new HostClient(worker, "unused", simulate: true, direct: direct), new() { MaxRetries = 0 });
        try
        {
            Assert.True(await camera.Connect(default));
            camera.EnableSubSample = true;
            camera.SubSampleWidth = 64;
            camera.SubSampleHeight = 32;
            camera.StartExposure(new CaptureSequence { ExposureTime = .01, Binning = new BinningMode(1, 1) });
            await camera.WaitUntilExposureIsReady(default);
            var exposure = Assert.IsType<ImageArrayExposureData>(await camera.DownloadExposure(default));
            Assert.Equal(model, exposure.MetaData.Camera.Name);
            // NINA also merges CameraInfo from its connected camera view model.
            // Exercise that path before using NINA's actual FITS header serializer.
            var metadata = new ImageMetaData();
            metadata.FromCameraInfo(new CameraInfo { Connected = true, DeviceId = camera.Id, Name = camera.Name, ReadoutModes = camera.ReadoutModes });
            var header = new FITSHeader(exposure.Width, exposure.Height);
            header.PopulateFromMetaData(metadata);
            using var stream = new MemoryStream();
            header.Write(stream);
            var cards = Encoding.ASCII.GetString(stream.ToArray()).Chunk(80).Select(c => new string(c)).ToArray();
            var instrument = Assert.Single(cards, c => c.StartsWith("INSTRUME="));
            Assert.Equal(model, instrument.Substring(10).Split('/')[0].Trim().Trim('\'').Trim());
            Assert.Equal("ZwoGain", exposure.MetaData.Camera.Id); // Saved NINA selection remains compatible.
            Assert.NotEqual(camera.DisplayName, exposure.MetaData.Camera.Name);
        }
        finally { camera.Disconnect(); }
    }
}
