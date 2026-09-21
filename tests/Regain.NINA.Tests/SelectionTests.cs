using Moq;
using NINA.Image.Interfaces;
using Regain.Core;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class SelectionTests : IDisposable
{
    private readonly string folder = Path.Combine(Path.GetTempPath(), "regain-selection-" + Guid.NewGuid().ToString("N"));
    private static readonly CameraDescriptor Sim = new("ZWO Simulated", 960, 640, true, 0, 3.76, 16, true, false, [1, 2, 4]);
    private CameraSelectionStore Store => new(Path.Combine(folder, "camera.json"));
    private ResilientCamera Camera(CameraSelectionStore store) => new(Mock.Of<IExposureDataFactory>(), store,
        () => new HostClient(Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../target/debug/regain-device.exe")), "unused", true), new());

    [Fact]
    public async Task SavedChoiceSurvivesNewInstancesAndLearnsSerial()
    {
        Store.Save(new(Sim));
        var store = Store;
        var camera = Camera(store);
        try
        {
            Assert.Equal("ZwoGain", camera.Id);
            Assert.True(await camera.Connect(default));
            Assert.Equal("ZWO Simulated", camera.Name);
            Assert.Equal(960, camera.CameraXSize);
            Assert.Equal("sim00001", Store.Load()!.Serial);
        }
        finally { camera.Disconnect(); }
        var reopened = Camera(Store);
        try { Assert.True(await reopened.Connect(default)); }
        finally { reopened.Disconnect(); }
    }

    [Fact]
    public async Task ConnectReadsNewSelectionAndNeverFallsBackFromMissingSerial()
    {
        var store = Store;
        store.Save(new(Sim with { Name = "Unavailable model", Width = 6248 }));
        var camera = Camera(store);
        store.Save(new(Sim)); // A dialog save after NINA created its provider object.
        try
        {
            Assert.True(await camera.Connect(default));
            Assert.Equal(960, camera.CameraXSize);
            camera.Disconnect();
            store.Save(new(Sim, "not-the-attached-camera"));
            await Assert.ThrowsAnyAsync<Exception>(() => camera.Connect(default));
            Assert.False(camera.Connected);
            Assert.Equal("not-the-attached-camera", Store.Load()!.Serial);
            Assert.Equal("ZwoGain", camera.Id);
        }
        finally { camera.Disconnect(); }
    }

    [Fact]
    public async Task NoSelectionRequiresSetupEvenWhenCameraIsAttached()
    {
        var camera = Camera(Store);
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => camera.Connect(default));
        Assert.Contains("setup dialog", error.Message);
        Assert.True(camera.HasSetupDialog);
    }

    [Fact]
    public void BackendDefaultsToSdkAndSurvivesSaveWithoutOverwritingANewerChoice()
    {
        var store = Store;
        store.Save(new(Sim));
        Assert.False(store.Load()!.UseDirectDriver);
        Assert.False(store.Load()!.AllowSdkFallback);
        var sdk = store.Load()!;
        store.Save(sdk with { UseDirectDriver = true, AllowSdkFallback = true });
        store.RememberSerial(sdk, "stale-connection");
        Assert.True(store.Load()!.UseDirectDriver);
        Assert.True(store.Load()!.AllowSdkFallback);
        Assert.Null(store.Load()!.Serial);
        // Existing installations have no backend field in camera.json.
        string file = Path.Combine(folder, "camera.json");
        File.WriteAllText(file, System.Text.Json.JsonSerializer.Serialize(new { Camera = Sim, Serial = "original" }));
        Assert.False(store.Load()!.UseDirectDriver);
        Assert.Equal("original", store.Load()!.Serial);
    }

    [Fact]
    public async Task DirectConnectionReplacesSavedSdkCapabilities()
    {
        var descriptor = Sim with { Name = "ZWO ASI676MC", Width = 3552, Height = 3552, Cooled = false };
        Store.Save(new(descriptor, UseDirectDriver: true));
        var camera = new ResilientCamera(Mock.Of<IExposureDataFactory>(), Store,
            () => new HostClient(Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../target/debug/regain-device.exe")),
                "missing-sdk.dll", simulate: true, direct: true), new());
        try {
            Assert.True(await camera.Connect(default));
            Assert.Equal(1, camera.MaxBinX);
            Assert.Equal(30, camera.ExposureMax);
            Assert.False(camera.CanSetUSBLimit);
            Assert.False(camera.CanSetTemperature);
            Assert.True(double.IsNaN(camera.Temperature));
            Assert.Contains("SDK-less", camera.DriverInfo);
            Assert.True(Store.Load()!.UseDirectDriver);
            Assert.Equal("direct-simulator", Store.Load()!.Serial);
        }
        finally { camera.Disconnect(); }
    }

    [Fact]
    public void ConnectionCannotOverwriteANewerDialogSelection()
    {
        var store = Store;
        var previous = new CameraSelection(Sim);
        store.Save(previous);
        store.Save(new(Sim with { Name = "ZWO ASI6200MM Pro" }));
        store.RememberSerial(previous, "sim00001");
        Assert.Equal("ZWO ASI6200MM Pro", Store.Load()!.Camera.Name);
        Assert.Null(Store.Load()!.Serial);
    }

    [Theory]
    [InlineData("ZWO ASI6200MM Pro",9576,6388)]
    [InlineData("ZWO ASI2600MM Pro",6248,4176)]
    public async Task P25SelectionAppliesAndPersistsFanAndLedOptions(string name, int width, int height)
    {
        var descriptor = new CameraDescriptor(name,width,height,false,0,3.76,16,true,false,[1,2,3,4]);
        Store.Save(new(descriptor,UseDirectDriver:true,FanSpeed:200,PowerLedBrightness:128));
        HostClient? host = null;
        var camera = new ResilientCamera(Mock.Of<IExposureDataFactory>(),Store,
            () => host = new HostClient(Path.GetFullPath(Path.Combine(AppContext.BaseDirectory,"../../../../../target/debug/regain-device.exe")),
                "missing-sdk.dll",simulate:true,direct:true),new());
        try {
            Assert.True(await camera.Connect(default));
            Assert.Equal(4,camera.MaxBinX); Assert.Equal(2000,camera.ExposureMax);
            Assert.True(camera.CanSetTemperature); Assert.True(camera.HasDewHeater);
            Assert.Equal(200,(await host!.CallAsync("get",new {control=22},TimeSpan.FromSeconds(5),default)).Result.GetInt32());
            Assert.Equal(128,(await host.CallAsync("get",new {control=23},TimeSpan.FromSeconds(5),default)).Result.GetInt32());
            Assert.Equal(200,Store.Load()!.FanSpeed); Assert.Equal(128,Store.Load()!.PowerLedBrightness);
        }
        finally {camera.Disconnect();}
    }

    public void Dispose()
    {
        if (Directory.Exists(folder)) Directory.Delete(folder, true);
    }
}
