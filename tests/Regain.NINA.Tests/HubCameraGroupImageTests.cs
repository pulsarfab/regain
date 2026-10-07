using System.Buffers.Binary;
using System.Globalization;
using System.Text;
using System.Text.Json;
using Moq;
using NINA.Core.Enum;
using NINA.Image.ImageAnalysis;
using NINA.Image.ImageData;
using NINA.Image.Interfaces;
using NINA.Profile.Interfaces;
using Regain.Hub;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    private static (HubCameraGroupImages Writer, Mock<IImageDataFactory> Factory) RealGroupImages(bool signed)
    {
        var cameraSettings = new Mock<ICameraSettings>(); cameraSettings.SetupGet(s => s.ASCOMCreate32BitData).Returns(signed);
        var fileSettings = new Mock<IImageFileSettings>(); fileSettings.SetupGet(s => s.FileType).Returns(FileTypeEnum.FITS);
        fileSettings.SetupGet(s => s.FITSUseLegacyWriter).Returns(true);
        var profiles = new Mock<IProfileService>(); profiles.Setup(p => p.ActiveProfile.CameraSettings).Returns(cameraSettings.Object);
        profiles.Setup(p => p.ActiveProfile.ImageFileSettings).Returns(fileSettings.Object);
        var analysis = new Mock<IStarDetectionAnalysis>(); analysis.SetupGet(a => a.HFR).Returns(double.NaN);
        analysis.SetupGet(a => a.DetectedStars).Returns(-1);
        var stars = new Mock<IStarDetection>(); stars.Setup(s => s.CreateAnalysis()).Returns(analysis.Object);
        var factory = new Mock<IImageDataFactory>(MockBehavior.Strict);
        factory.Setup(f => f.CreateBaseImageData(It.IsAny<IImageArray>(), It.IsAny<int>(), It.IsAny<int>(), It.IsAny<int>(), It.IsAny<bool>(), It.IsAny<ImageMetaData>()))
            .Returns((IImageArray pixels, int width, int height, int depth, bool bayer, ImageMetaData metadata) =>
                new BaseImageData(pixels, width, height, depth, bayer, metadata, profiles.Object, stars.Object, Mock.Of<IStarAnnotator>()));
        var writer = new HubCameraGroupImages(factory.Object, profiles.Object);
        return (writer, factory);
    }
    [Theory, InlineData(false), InlineData(true)]
    public async Task CameraGroupNinaWriterSavesExactHistoricalPixelsAndAuthoritativeMetadata(bool signed)
    {
        await using var host = await Host.Open(PairedCameras);
        using var groups = await HubCameraGroups.AttachAsync(host.Executable, host.ConfigPath, host.Client.Hello.InstanceId);
        var group = groups.Groups.Single().GetProperty("id").GetGuid(); var demands = groups.UniformRequests(group, 0.2, true, requireScalarImage: true);
        var started = await groups.StartAsync(group, demands); var operation = started.GetProperty("operation").GetGuid(); JsonElement result = default;
        await Eventually(async () => { result = await groups.StatusAsync(group, operation); return HubCameraGroups.Terminal(result); });
        Assert.Equal("complete", result.GetProperty("phase").GetString());
        Assert.All(result.GetProperty("result").GetProperty("members").EnumerateArray(), member => {
            Assert.True(member.GetProperty("requireScalarImage").GetBoolean());
            Assert.Equal(65535, member.GetProperty("captureProfile").GetProperty("maxAdu").GetInt32());
            Assert.Equal(0, member.GetProperty("captureProfile").GetProperty("sensorType").GetInt32());
        });
        // Change the current source format after this historical image is pinned.
        // Saving must use frozen preflight metadata, not this later live setting.
        var output = host.Config["outputs"]![0]!["id"]!.GetValue<Guid>();
        await host.Command(new { op = "connect", output });
        await host.Command(new { op = "put", output, property = HubCameraProtocol.Setting(HubCameraProperty.ReadoutMode, 1) });
        await host.Command(new { op = "disconnect", output });
        var (writer, factory) = RealGroupImages(signed);
        var frame = await writer.DownloadAsync(groups, group, operation, demands[0].Source, CancellationToken.None);
        Assert.Equal(signed ? typeof(int[]) : typeof(ushort[]), frame.Pixels.GetType()); Assert.Equal(16, frame.BitDepth);
        Assert.Equal(0.2, frame.MetaData.Image.ExposureTime, 5); Assert.Equal(DateTimeKind.Utc, frame.MetaData.Image.ExposureStart.Kind);
        Assert.Equal(SensorType.Monochrome, frame.MetaData.Camera.SensorType); Assert.Equal(0, frame.MetaData.Camera.BayerOffsetX);
        var settings = writer.SaveSettings(); settings.FilePath = host.DirectoryPath; settings.FilePattern = "regain-" + operation.ToString("N");
        var path = await writer.SaveAsync(groups, group, operation, demands[0].Source, settings, CancellationToken.None);
        Assert.True(File.Exists(path)); var file = new FileInfo(path); Assert.InRange(file.Length, 100, 1024 * 1024);
        var bytes = await File.ReadAllBytesAsync(path); var cards = new Dictionary<string, string>(); var offset = 0;
        while (offset + 80 <= bytes.Length) {
            var card = Encoding.ASCII.GetString(bytes, offset, 80); offset += 80; var key = card.Substring(0, 8).Trim();
            if (key == "END") break;
            if (card[8] == '=') cards[key] = card.Substring(10).Split('/')[0].Trim().Trim('\'').Trim();
        }
        offset = (offset + 2879) / 2880 * 2880;
        Assert.Equal("320", cards["NAXIS1"]); Assert.Equal("240", cards["NAXIS2"]);
        Assert.Equal(operation.ToString(), cards["RGNOP"]); Assert.Equal(group.ToString(), cards["RGNGROUP"]);
        Assert.Equal(result.GetProperty("result").GetProperty("members")[0].GetProperty("acquisition").GetString(), cards["RGNACQ"]);
        Assert.Equal(0.2, double.Parse(cards["EXPTIME"], CultureInfo.InvariantCulture), 5);
        var bitpix = int.Parse(cards["BITPIX"], CultureInfo.InvariantCulture); Assert.True(bitpix is 16 or 32);
        var zero = cards.TryGetValue("BZERO", out var bzero) ? double.Parse(bzero, CultureInfo.InvariantCulture) : 0;
        foreach (var index in new[] { 0, 1, 319, 320, 320 * 240 - 1 }) {
            var stored = bitpix == 16 ? BinaryPrimitives.ReadInt16BigEndian(bytes.AsSpan(offset + index * 2, 2)) : BinaryPrimitives.ReadInt32BigEndian(bytes.AsSpan(offset + index * 4, 4));
            Assert.Equal(Convert.ToDouble(frame.Pixels.GetValue(index)), stored + zero);
        }
        factory.Verify(f => f.CreateBaseImageData(It.IsAny<IImageArray>(), 320, 240, 16, false, It.IsAny<ImageMetaData>()), Times.Once);
    }
}
