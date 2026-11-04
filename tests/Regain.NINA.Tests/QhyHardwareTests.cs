using System.Diagnostics;
using System.Security.Cryptography;
using System.Runtime.InteropServices;
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

public sealed class QhyHardwareTheoryAttribute : TheoryAttribute {
    public QhyHardwareTheoryAttribute() {
        if (Environment.GetEnvironmentVariable("REGAIN_TEST_QHY600_HARDWARE") != "1")
            Skip = "Requires an idle QHY600M; opt in with REGAIN_TEST_QHY600_HARDWARE=1";
    }
}

public sealed class QhyHardwareTests(ITestOutputHelper output) {
    [QhyHardwareTheory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task FullFramesCoolingCancelAndMetadata(bool direct) {
        const int width=9600, height=6422;
        const string model="QHY600M", serial="qhy600m-96763b4ab6a79e117";
        string root=Path.GetFullPath(Path.Combine(AppContext.BaseDirectory,"../../../../../"));
        string settings=Path.Combine(Path.GetTempPath(),"regain-qhy-"+Guid.NewGuid().ToString("N"),"camera.json");
        var store=new CameraSelectionStore(settings);
        store.Save(new(new(model,width,height,false,0,3.76,16,true,false,[1]),serial,UseDirectDriver:direct,SaveCameraSerialNumber:true));
        HostClient? host=null;
        var hashes=new HashSet<string>();
        var factory=new Mock<IExposureDataFactory>();
        factory.Setup(f=>f.CreateImageArrayExposureData(It.IsAny<ushort[]>(),It.IsAny<int>(),It.IsAny<int>(),It.IsAny<int>(),It.IsAny<bool>(),It.IsAny<ImageMetaData>()))
            .Returns((ushort[] pixels,int w,int h,int bits,bool color,ImageMetaData metadata)=> {
                Assert.Equal((width,height,16,false),(w,h,bits,color));
                Assert.Equal(width*height,pixels.Length);
                Assert.Equal(model,metadata.Camera.Name);
                Assert.Equal(5,metadata.Image.ExposureTime);
                Assert.True(hashes.Add(Convert.ToHexString(SHA256.HashData(MemoryMarshal.AsBytes(pixels.AsSpan())))),"Stale full frame");
                return new ImageArrayExposureData(pixels,w,h,bits,color,metadata,Mock.Of<IImageDataFactory>());
            });
        var camera=new ResilientCamera(factory.Object,store,
            ()=>host=new HostClient(Path.Combine(root,"target/release/regain-alpaca.exe"),Path.Combine(root,"vendor/zwo/ASICamera2.dll"),direct:direct,supervised:true,log:output.WriteLine),
            new() {MaxRetries=0,DirectReadRetries=0,ReadyFrameDownloadRetries=0,UsbResetAfterFailures=0,TransferLogMode="circular"});
        using var deadline=new CancellationTokenSource(TimeSpan.FromMinutes(4));
        try {
            Assert.True(await camera.Connect(deadline.Token));
            Assert.Equal(3600, camera.ExposureMax);
            Assert.Equal(.000001, camera.ExposureMin);
            Assert.False(camera.HasDewHeater);
            var state=(await host!.CallAsync("diagnostics",null,TimeSpan.FromSeconds(10),deadline.Token)).Result;
            Assert.Equal(serial,state.GetProperty("serial").GetString());
            int worker=state.GetProperty("processId").GetInt32();
            using(var process=Process.GetProcessById(worker)) {
                bool loaded=process.Modules.Cast<ProcessModule>().Any(m=>m.ModuleName.Equals("qhyccd.dll",StringComparison.OrdinalIgnoreCase));
                Assert.Equal(!direct,loaded);
            }
            double baseline=camera.Temperature;
            camera.TemperatureSetPoint=-5;
            camera.CoolerOn=true;
            for(int frame=0;frame<3;frame++) {
                if(frame==1) await camera.UpdateRuntimeOptionsAsync(new(){TransferLogMode="off",CoolerMaxPowerChangePer5Seconds=10});
                if(frame==2) await camera.UpdateRuntimeOptionsAsync(new(){TransferLogMode="circular",CoolerMaxPowerChangePer5Seconds=25});
                await Capture();
            }
            // Keep sampling long enough to verify signed temperature and actual
            // cooling while idle. This is a thermal test, not a long exposure.
            for(int second=0;second<100 && camera.Temperature>-.5;second++) {
                await Task.Delay(1000,deadline.Token);
                Assert.True(double.IsFinite(camera.Temperature));
                Assert.InRange(camera.CoolerPower,0,100);
                if(second%10==0) output.WriteLine($"Cooling {camera.Temperature:F1} C, {camera.CoolerPower:F1}% (baseline {baseline:F1} C)");
            }
            Assert.True(camera.Temperature<0,$"Did not reach negative temperature: {camera.Temperature}");
            camera.StartExposure(Sequence());
            var ready=camera.WaitUntilExposureIsReady(deadline.Token);
            await Task.Delay(1200,deadline.Token);
            camera.AbortExposure();
            Assert.NotNull(await Record.ExceptionAsync(()=>ready.WaitAsync(TimeSpan.FromSeconds(15))));
            await Capture();
            state=(await host.CallAsync("diagnostics",null,TimeSpan.FromSeconds(10),deadline.Token)).Result;
            Assert.Equal(worker,state.GetProperty("processId").GetInt32());
            Assert.Equal(0,state.GetProperty("retry").GetProperty("recaptures").GetInt32());
            output.WriteLine($"{(direct?"Direct USB":"SDK")}: four full frames, signed cooling, live options, cancellation/restart, stable worker {worker}");
        } finally {
            camera.Disconnect();
            factory.Invocations.Clear();
            if(File.Exists(settings)) File.Delete(settings);
            if(Directory.Exists(Path.GetDirectoryName(settings))) Directory.Delete(Path.GetDirectoryName(settings)!);
        }
        async Task Capture() {
            var clock=Stopwatch.StartNew();
            camera.StartExposure(Sequence());
            await camera.WaitUntilExposureIsReady(deadline.Token).WaitAsync(TimeSpan.FromSeconds(35));
            var frame=Assert.IsType<ImageArrayExposureData>(await camera.DownloadExposure(deadline.Token));
            Assert.Equal((width,height),(frame.Width,frame.Height));
            output.WriteLine($"Full frame {hashes.Count}: {clock.Elapsed.TotalSeconds:F2} s, {camera.Temperature:F1} C, {camera.CoolerPower:F1}%");
            factory.Invocations.Clear();
        }
        static CaptureSequence Sequence()=>new(){ExposureTime=5,Gain=30,Offset=30,Binning=new BinningMode(1,1)};
    }
}
