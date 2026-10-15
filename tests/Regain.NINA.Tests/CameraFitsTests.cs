using System.Text;
using Moq;
using NINA.Core.Model.Equipment;
using NINA.Equipment.Equipment.MyCamera;
using NINA.Equipment.Model;
using NINA.Equipment.Utility;
using NINA.Image.FileFormat.FITS;
using NINA.Image.FileFormat.XISF;
using NINA.Image.ImageData;
using NINA.Image.Interfaces;
using Regain.Core;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class CameraFitsTests
{
    [Theory]
    [InlineData(false, false)]
    [InlineData(false, true)]
    [InlineData(true, false)]
    [InlineData(true, true)]
    public async Task CameraSerialIsOptInAndUsesConnectedHardwareIdentity(bool direct, bool enabled)
    {
        var images = new Mock<IExposureDataFactory>();
        images.Setup(f => f.CreateImageArrayExposureData(It.IsAny<ushort[]>(), It.IsAny<int>(), It.IsAny<int>(), It.IsAny<int>(), It.IsAny<bool>(), It.IsAny<ImageMetaData>()))
            .Returns((ushort[] pixels, int w, int h, int depth, bool color, ImageMetaData metadata) =>
                new ImageArrayExposureData(pixels, w, h, depth, color, metadata, Mock.Of<IImageDataFactory>()));
        string worker = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../target/debug/regain-device.exe"));
        string root = Path.Combine(Path.GetTempPath(), "regain-serial-" + Guid.NewGuid().ToString("N"));
        var store = new CameraSelectionStore(Path.Combine(root, "camera.json"));
        string model = direct ? "ZWO ASI585MM Pro" : "ZWO Simulated";
        store.Save(new(new(model, direct ? 3840 : 960, direct ? 2160 : 640, !direct, 0, 2.9, 16, true, false, [1]),
            UseDirectDriver: direct, SaveCameraSerialNumber: enabled));
        var camera = new ResilientCamera(images.Object, store,
            () => new HostClient(worker, "unused", simulate: true, direct: direct), new() { MaxRetries = 0 });
        try
        {
            Assert.True(await camera.Connect(default));
            string serial = camera.SerialNumber;
            Assert.False(string.IsNullOrWhiteSpace(serial));
            // The next connection's saved selection must not change this frame's identity.
            store.Save(store.Load()! with { Serial = "0000000000009999", SaveCameraSerialNumber = !enabled });
            camera.StartExposure(new CaptureSequence { ExposureTime = .01, Binning = new BinningMode(1, 1) });
            await camera.WaitUntilExposureIsReady(default);
            var exposure = Assert.IsType<ImageArrayExposureData>(await camera.DownloadExposure(default));
            var header = new FITSHeader(exposure.Width, exposure.Height);
            header.PopulateFromMetaData(exposure.MetaData);
            var serials = header.ExtractMetaData().GenericHeaders.OfType<StringMetaDataHeader>().Where(h => h.Key == "CAMSER").ToArray();
            if (enabled)
                Assert.Equal(serial, Assert.Single(serials).Value);
            else
                Assert.Empty(serials);
            var xisf = new XISFHeader();
            xisf.AddImageMetaData(new ImageProperties(exposure.Width, exposure.Height, 16, false, 0, 0), "DARK");
            xisf.Populate(exposure.MetaData);
            var xisfSerials = xisf.ExtractMetaData().GenericHeaders.OfType<StringMetaDataHeader>().Where(h => h.Key == "CAMSER").ToArray();
            if (enabled)
                Assert.Equal(serial, Assert.Single(xisfSerials).Value);
            else
                Assert.Empty(xisfSerials);
            Assert.Equal("ZwoGain", exposure.MetaData.Camera.Id);
            Assert.Equal(model, exposure.MetaData.Camera.Name);
        }
        finally
        {
            camera.Disconnect();
            if (Directory.Exists(root)) Directory.Delete(root, true);
        }
    }

    [Fact]
    public void OlderCameraSelectionsDefaultToNoSerialMetadata()
    {
        var selection = System.Text.Json.JsonSerializer.Deserialize<CameraSelection>(
            "{\"Camera\":{\"Name\":\"ZWO ASI585MM Pro\"},\"Serial\":\"0000000000000001\"}")!;
        Assert.False(selection.SaveCameraSerialNumber);
        Assert.Equal("0000000000000001", selection.Serial);
    }

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
