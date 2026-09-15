using System.ComponentModel.Composition;
using Moq;
using NINA.Core.Model.Equipment;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.ViewModel;
using NINA.Equipment.Model;
using NINA.Image.ImageData;
using NINA.Image.Interfaces;
using NINA.Profile.Interfaces;
using ZwoGain.Core;
using Xunit;

namespace ZwoGain.NINA.Tests;

public class CameraTests
{
    [Theory]
    [InlineData(1)] [InlineData(2)] [InlineData(3)] [InlineData(4)]
    public async Task DirectRoiUsesAlignedRectangleInNinaAndImageMetadata(short bin)
    {
        string root=Path.GetFullPath(Path.Combine(AppContext.BaseDirectory,"../../../../../"));
        var images=new Mock<IExposureDataFactory>();
        images.Setup(f=>f.CreateImageArrayExposureData(It.IsAny<ushort[]>(),It.IsAny<int>(),It.IsAny<int>(),It.IsAny<int>(),It.IsAny<bool>(),It.IsAny<ImageMetaData>()))
            .Returns((ushort[] p,int w,int h,int depth,bool color,ImageMetaData m)=>new ImageArrayExposureData(p,w,h,depth,color,m,Mock.Of<IImageDataFactory>()));
        var camera=new ResilientCamera(new("ZWO ASI6200MM Pro",9576,6388,false,0,3.76,16,true,false,[1,2,3,4]),
            images.Object,()=>new HostClient(Path.Combine(root,"target/debug/zwogain-direct.exe"),"unused",simulate:true,direct:true),new(){MaxRetries=0});
        try {
            await camera.Connect(default);
            camera.EnableSubSample=true;
            camera.SubSampleX=17; camera.SubSampleY=3;
            camera.SubSampleWidth=camera.SubSampleHeight=64;
            camera.StartExposure(new CaptureSequence{ExposureTime=.01,Binning=new BinningMode(bin,bin)});
            await camera.WaitUntilExposureIsReady(default);
            var frame=Assert.IsType<ImageArrayExposureData>(await camera.DownloadExposure(default));
            Assert.Equal(0,camera.SubSampleX%16); Assert.Equal(0,camera.SubSampleY%2);
            Assert.Equal(camera.SubSampleWidth,frame.Width*bin);
            Assert.Equal(camera.SubSampleHeight,frame.Height*bin);
            Assert.Equal(camera.SubSampleX/bin%2,frame.MetaData.Camera.BayerOffsetX);
            Assert.Equal(bin,frame.MetaData.Camera.BinX);
        } finally {camera.Disconnect();}
    }
    [Fact]
    public void PluginManifestUsesEmbeddedLogoAndApacheLicense()
    {
        var plugin = new ZwoGainPlugin();
        Assert.Equal("ZWOgain", plugin.Name);
        Assert.Equal("Apache-2.0", plugin.License);
        Assert.Equal("pack://application:,,,/ZwoGain.NINA;component/Assets/zwogain.png", plugin.Descriptions.FeaturedImageURL);
        var resources = new System.Resources.ResourceManager("ZwoGain.NINA.g", typeof(ZwoGainPlugin).Assembly);
        using var stream = resources.GetStream("assets/zwogain.png");
        Assert.NotNull(stream);
        Span<byte> header = stackalloc byte[24];
        stream.ReadExactly(header);
        Assert.Equal("89504E470D0A1A0A", Convert.ToHexString(header[..8]));
        Assert.Equal(256, System.Buffers.Binary.BinaryPrimitives.ReadInt32BigEndian(header[16..20]));
        Assert.Equal(256, System.Buffers.Binary.BinaryPrimitives.ReadInt32BigEndian(header[20..24]));
        Assert.Equal(typeof(ZwoGainPlugin).Assembly.GetName().Version, typeof(CameraSession).Assembly.GetName().Version);
    }
    [Fact]
    public void ExportsCameraProvider()
    {
        Assert.Contains(typeof(CameraProvider).GetCustomAttributes(typeof(ExportAttribute), false).Cast<ExportAttribute>(), a => a.ContractType == typeof(IEquipmentProvider));
        Assert.Contains(typeof(ICamera), typeof(ResilientCamera).GetInterfaces());
    }
    [Theory]
    [InlineData(false, false, .01)]
    [InlineData(true, false, .01)]
    [InlineData(false, true, 1200)]
    public async Task NinaLifecycleHidesDownloadFailureAndPreservesCaptureMetadata(bool sdkClampsOffset, bool reread, double seconds)
    {
        string root = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../"));
        int starts = 0;
        var loggedFailure = new TaskCompletionSource<string>(TaskCreationOptions.RunContinuationsAsynchronously);
        var settings = new Mock<ICameraSettings>();
        settings.SetupProperty(s => s.Timeout, 1);
        var profiles = new Mock<IProfileService>();
        profiles.Setup(p => p.ActiveProfile.CameraSettings).Returns(settings.Object);
        var factory = new Mock<IExposureDataFactory>();
        factory.Setup(f => f.CreateImageArrayExposureData(It.IsAny<ushort[]>(), It.IsAny<int>(), It.IsAny<int>(), It.IsAny<int>(), It.IsAny<bool>(), It.IsAny<ImageMetaData>()))
            .Returns((ushort[] p, int w, int h, int b, bool color, ImageMetaData m) => new ImageArrayExposureData(p, w, h, b, color, m, Mock.Of<IImageDataFactory>()));
        HostClient Host()
        {
            var host = new HostClient(Path.Combine(root, "target/debug/zwogain-host.exe"), "unused", true, log: line =>
                CameraLog.Forward(CameraLog.Parse("SDK", line), _ => { }, text => loggedFailure.TrySetResult(text), _ => { }));
            if (reread) host.CallAsync("simulation", new { instant = true }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
            if (sdkClampsOffset)
                host.CallAsync("simulation", new { clampControl = 5, clampMinimum = 20 }, TimeSpan.FromSeconds(15), default).GetAwaiter().GetResult();
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
                ReadyFrameDownloadRetries = reread ? 2 : 0,
                ReconnectDelaySeconds = 1.2,
                CoolingStableSamples = 1,
                CoolingSampleSeconds = .01
            }, profiles.Object);
        try
        {
            Assert.True(await camera.Connect(default));
            camera.StartExposure(new CaptureSequence { ExposureTime = seconds, Gain = 123, Offset = 17, Binning = new BinningMode(2, 2) });
            Assert.True(settings.Object.Timeout > 1);
            // Model NINA's outer readiness deadline. Recovery exceeds the original limit.
            using var ninaDeadline = new CancellationTokenSource(TimeSpan.FromSeconds(seconds + settings.Object.Timeout));
            await camera.WaitUntilExposureIsReady(ninaDeadline.Token);
            camera.Gain = 222; // Image metadata must represent the completed exposure, not the next one.
            var image = Assert.IsType<ImageArrayExposureData>(await camera.DownloadExposure(default));
            Assert.Equal(1, settings.Object.Timeout);
            Assert.Equal(480, image.Width);
            Assert.Equal(320, image.Height);
            Assert.Equal(16, image.BitDepth);
            Assert.True(image.IsBayered);
            Assert.Equal(123, image.MetaData.Camera.Gain);
            Assert.Equal(sdkClampsOffset ? 20 : 17, image.MetaData.Camera.Offset);
            Assert.Equal(2, image.MetaData.Camera.BinX);
            Assert.Equal(seconds, image.MetaData.Image.ExposureTime);
            Assert.NotEqual(DateTime.MinValue, image.MetaData.Image.ExposureStart);
            Assert.True(camera.Connected);
            Assert.Equal(reread ? 1 : 2, starts);
            var diagnostic = await loggedFailure.Task.WaitAsync(TimeSpan.FromSeconds(3));
            Assert.Contains("[command.failed]", diagnostic);
            Assert.Contains("ASI error 11", diagnostic);
        }
        finally { camera.Disconnect(); }
    }
    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task CancelRestoresNinaTimeoutWithoutOverwritingAUserEdit(bool userEdited)
    {
        string root = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../"));
        var settings = new Mock<ICameraSettings>();
        settings.SetupProperty(s => s.Timeout, 60);
        var profiles = new Mock<IProfileService>();
        profiles.Setup(p => p.ActiveProfile.CameraSettings).Returns(settings.Object);
        var camera = new ResilientCamera(new("ZWO Simulated", 960, 640, true, 0, 3.76, 16, true, false, [1, 2, 4]),
            Mock.Of<IExposureDataFactory>(), () => new HostClient(Path.Combine(root, "target/debug/zwogain-host.exe"), "unused", true), new(), profiles.Object);
        try
        {
            Assert.True(await camera.Connect(default));
            camera.StartExposure(new CaptureSequence { ExposureTime = 10, Binning = new BinningMode(1, 1) });
            Assert.True(settings.Object.Timeout > 60);
            if (userEdited) settings.Object.Timeout = 90;
            camera.AbortExposure();
            await Assert.ThrowsAnyAsync<OperationCanceledException>(() => camera.WaitUntilExposureIsReady(default));
            Assert.Equal(userEdited ? 90 : 60, settings.Object.Timeout);
        }
        finally { camera.Disconnect(); }
    }
}
