using Regain.Core;
using Xunit;

namespace Regain.Tests;

public class ReviewRegressionTests
{
    static readonly string Root = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory,"../../../../../"));
    static readonly CameraDescriptor Camera = new("ZWO ASI6200MM Pro",9576,6388,false,0,3.76,16,true,false,[1,2,3,4]);
    static readonly Exposure Request = new(64,64,1,0,0,10000,true);
    static HostClient Direct() => new(Path.Combine(Root,"target/debug/regain-direct.exe"),"unused",simulate:true,direct:true);
    static async Task Until(Func<bool> done) {
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(5));
        while (!done()) await Task.Delay(10,deadline.Token);
    }

    [Fact]
    public async Task AbortRestoresIdleControlsWithoutAnotherExposure()
    {
        HostClient? current=null;
        int starts=0;
        using var session=new CameraSession(Camera,()=>{ Interlocked.Increment(ref starts); return current=Direct(); },
            new(){MaxRetries=0,ReconnectDelaySeconds=.05});
        await session.ConnectAsync(default);
        session.Set(16,0); session.Set(17,1); await session.RefreshAsync(default);
        using var cancel=new CancellationTokenSource();
        session.Diagnostic+=phase=>{if(phase=="Exposing") cancel.Cancel();};
        await Assert.ThrowsAnyAsync<OperationCanceledException>(()=>session.CaptureAsync(Request with {microseconds=2000000},cancel.Token));
        session.Set(17,0);
        await Until(()=>Volatile.Read(ref starts)==2 && session.ControlConnectionAvailable);
        await session.RefreshAsync(default);
        Assert.Equal(0,(await current!.CallAsync("get",new{control=17},TimeSpan.FromSeconds(2),default)).Result.GetInt64());
        Assert.Equal(0,(await current.CallAsync("get",new{control=16},TimeSpan.FromSeconds(2),default)).Result.GetInt64());
        Assert.Equal(0,(await current.CallAsync("status",null,TimeSpan.FromSeconds(2),default)).Result.GetInt32());
    }

    [Theory]
    [InlineData(.2, true)]
    [InlineData(.02, false)]
    public async Task DirectReadoutUsesDownloadBudgetAndStillExpires(double downloadBudget, bool success)
    {
        using var session=new CameraSession(Camera,()=>{
            var host=Direct();
            host.CallAsync("simulation",new{readDelayMs=120},TimeSpan.FromSeconds(2),default).GetAwaiter().GetResult();
            return host;
        },new(){MaxRetries=0,MaximumRetryExposureSeconds=0,ExposureGraceSeconds=.02,DownloadTimeoutSeconds=downloadBudget,DirectReadRetries=0,ReconnectDelaySeconds=.05});
        await session.ConnectAsync(default);
        if (success) Assert.Equal(4096,(await session.CaptureAsync(Request,default)).Pixels.Length);
        else await Assert.ThrowsAsync<IOException>(()=>session.CaptureAsync(Request,default));
    }

    [Fact]
    public async Task DisconnectAfterAbortOpensCleanupWorkerAndLeavesNoWorkerRunning()
    {
        var pids=new System.Collections.Concurrent.ConcurrentQueue<int>();
        using var session=new CameraSession(Camera,()=>{
            var host=Direct(); pids.Enqueue(host.ProcessId); return host;
        },new(){MaxRetries=0,ReconnectDelaySeconds=.2});
        await session.ConnectAsync(default);
        session.Set(17,1); await session.RefreshAsync(default);
        using var cancel=new CancellationTokenSource();
        session.Diagnostic+=phase=>{if(phase=="Exposing") cancel.Cancel();};
        await Assert.ThrowsAnyAsync<OperationCanceledException>(()=>session.CaptureAsync(Request with{microseconds=2000000},cancel.Token));
        session.Dispose();
        Assert.Equal(2,pids.Count);
        foreach(int pid in pids) {
            try { using var process=System.Diagnostics.Process.GetProcessById(pid); Assert.True(process.HasExited); }
            catch(ArgumentException) { /* Process has already been reaped. */ }
        }
    }

    [Fact]
    public async Task IdleReconnectionDoesNotSkipThermalSettlingForNextCapture()
    {
        int starts=0, exposures=0;
        var descriptor=new CameraDescriptor("ZWO Simulated",960,640,true,0,3.76,16,true,false,[1,2,4]);
        using var session=new CameraSession(descriptor,()=>{
            var host=new HostClient(Path.Combine(Root,"target/debug/regain-host.exe"),"unused",simulate:true);
            if(Interlocked.Increment(ref starts)>1)
                host.CallAsync("simulation",new{temperature=100},TimeSpan.FromSeconds(2),default).GetAwaiter().GetResult();
            return host;
        },new(){MaxRetries=0,ReconnectDelaySeconds=.05,CoolingTimeoutSeconds=.08,CoolingSampleSeconds=.01,CoolingStableSamples=1});
        session.Diagnostic+=phase=>{if(phase=="Starting exposure") Interlocked.Increment(ref exposures);};
        await session.ConnectAsync(default);
        using var cancel=new CancellationTokenSource();
        session.Diagnostic+=phase=>{if(phase=="Exposing") cancel.Cancel();};
        await Assert.ThrowsAnyAsync<OperationCanceledException>(()=>session.CaptureAsync(Request with{microseconds=2000000},cancel.Token));
        await Until(()=>Volatile.Read(ref starts)==2 && session.ControlConnectionAvailable);
        await Assert.ThrowsAsync<IOException>(()=>session.CaptureAsync(Request,default));
        Assert.Equal(1,exposures);
    }

    [Fact]
    public async Task CleanupFailureDeliversFrameAndReconnectsBeforeNextExposure()
    {
        int starts=0;
        using var session=new CameraSession(Camera,()=>{
            var host=Direct();
            if (Interlocked.Increment(ref starts)==1)
                host.CallAsync("simulation",new{cleanupFailure=true},TimeSpan.FromSeconds(2),default).GetAwaiter().GetResult();
            return host;
        },new(){MaxRetries=0,MaximumRetryExposureSeconds=0,ReconnectDelaySeconds=.05});
        await session.ConnectAsync(default);
        var frame=await session.CaptureAsync(Request,default);
        Assert.Equal(0,frame.Recoveries);
        Assert.Equal((ushort)4095,frame.Pixels[^1]);
        Assert.Contains("cleanup",session.LastError);
        await Until(()=>Volatile.Read(ref starts)==2 && session.ControlConnectionAvailable);
        Assert.Equal((ushort)4095,(await session.CaptureAsync(Request,default)).Pixels[^1]);
        Assert.Equal(2,starts);
    }

    [Fact]
    public async Task SelectedSdkSerialCanFollowBusyCameraButNeverSubstitutesAnother()
    {
        HostClient Sdk() => new(Path.Combine(Root,"target/debug/regain-host.exe"),Path.Combine(Root,"target/debug/selection_sdk.dll"));
        using(var host=Sdk()) {
            var result=await host.CallAsync("open",new{name="Review Twin",serial="0202020202020202"},TimeSpan.FromSeconds(2),default);
            Assert.Equal("0202020202020202",result.Result.GetProperty("serial").GetString());
        }
        using(var host=Sdk())
            await Assert.ThrowsAsync<SdkException>(()=>host.CallAsync("open",new{name="Review Twin",serial="0303030303030303"},TimeSpan.FromSeconds(2),default));
    }

    [Theory]
    [InlineData(1)] [InlineData(2)] [InlineData(3)] [InlineData(4)]
    public async Task RoiNormalizationFitsSmallAndEdgeRectanglesWithoutFallback(int bin)
    {
        using var session=new CameraSession(Camera,Direct,new(){MaxRetries=0});
        await session.ConnectAsync(default);
        foreach(var roi in new[]{Request with{width=64/bin,height=64/bin,x=17/bin,y=3/bin,bin=bin},
            Request with{width=64/bin,height=64/bin,x=9575/bin,y=6387/bin,bin=bin}}) {
            var adjusted=session.Camera.NormalizeRoi(roi);
            Assert.Equal(0,adjusted.x*bin%16); Assert.Equal(0,adjusted.y*bin%2);
            Assert.InRange(adjusted.width*bin,64,9576); Assert.InRange(adjusted.height*bin,64,6388);
            Assert.True((adjusted.x+adjusted.width)*bin<=9576);
            Assert.True((adjusted.y+adjusted.height)*bin<=6388);
            Assert.Equal(adjusted,(await session.CaptureAsync(adjusted,default)).Exposure);
            Assert.Equal("direct",session.Backend);
        }
    }
}
