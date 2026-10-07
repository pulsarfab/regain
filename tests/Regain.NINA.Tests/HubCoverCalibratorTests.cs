using System.Text.Json;
using System.Diagnostics;
using System.Net;
using System.Net.Http;
using System.Net.Sockets;
using NINA.Equipment.Interfaces;
using Regain.Hub;
using Regain.TestFixtures;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    [Fact]
    public async Task ActualPanelPublisherAndNativeNinaShareLightWithoutOwningEachOthersLeases()
    {
        using var source = new HubCoverCalibratorServer();
        await using var host = await Host.Open(config => source.AddTo(config,4,17));
        using var device = new HubCoverCalibratorDevice(host.Selection(3,"covercalibrator"),host.Executable,host.Workers);
        await device.Connect(CancellationToken.None);
        var reservation = new TcpListener(IPAddress.Loopback,0); reservation.Start();
        var port = ((IPEndPoint)reservation.LocalEndpoint).Port; reservation.Stop();
        var profiles = Path.Combine(host.DirectoryPath,"empty-panel-camera-profiles.json");
        await File.WriteAllTextAsync(profiles,"[]");
        using var publisher = new Process { StartInfo = new ProcessStartInfo(host.Executable) {
            UseShellExecute=false,CreateNoWindow=true,WindowStyle=ProcessWindowStyle.Hidden,
            RedirectStandardOutput=true,RedirectStandardError=true } };
        foreach (var argument in new[]{"--hub-config",host.ConfigPath,"--profiles",profiles,"--simulate","--workers",host.Workers,"--no-discovery","--port",port.ToString()}) publisher.StartInfo.ArgumentList.Add(argument);
        Assert.True(publisher.Start());
        using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(10));
        var output = publisher.StandardOutput.ReadToEndAsync(timeout.Token); var errors = publisher.StandardError.ReadToEndAsync(timeout.Token);
        Exception? primaryFailure = null;
        try {
            using var http = new HttpClient { BaseAddress=new Uri("http://127.0.0.1:"+port),Timeout=TimeSpan.FromSeconds(3) };
            async Task<JsonElement> Call(string member,string? key = null,string? value = null)
            {
                using var response = key is null ? await http.GetAsync("/api/v1/covercalibrator/17/"+member+"?ClientID=87001",timeout.Token) :
                    await http.PutAsync("/api/v1/covercalibrator/17/"+member,new FormUrlEncodedContent(new Dictionary<string,string>{["ClientID"]="87001",[key]=value!}),timeout.Token);
                response.EnsureSuccessStatusCode(); using var parsed=JsonDocument.Parse(await response.Content.ReadAsStringAsync(timeout.Token));
                var number = parsed.RootElement.GetProperty("ErrorNumber").GetInt32();
                string? diagnostic = null;
                if (number != 0) {
                    try { diagnostic = (await host.Command(new {op="sourceStatus",source=source.SourceId})).GetRawText(); }
                    catch (Exception failure) { diagnostic = "Source diagnostic failed: " + failure.GetType().Name; }
                }
                Assert.True(number == 0,$"Panel {member} failed: {parsed.RootElement.GetRawText()}; source={diagnostic}; upstream={source.RequestTrace}");
                return parsed.RootElement.GetProperty("Value").Clone();
            }
            await Eventually(async () => {
                if (publisher.HasExited) throw new InvalidOperationException("Private panel publisher exited before readiness");
                try { return (await Call("connected")).ValueKind is JsonValueKind.True or JsonValueKind.False; }
                catch (HttpRequestException) { return false; }
            });
            await Call("connected","Connected","true");
            Assert.Equal(2,(await host.Command(new {op="sourceStatus",source=source.SourceId})).GetProperty("leaseCount").GetInt32());
            var zero = Task.Run(() => device.Brightness=0);
            await Eventually(() => source.PanelCommands.Count==1);
            source.Values["calibratorstate"]=3; source.Values["calibratorchanging"]=false; await zero;
            Assert.Equal(0,(await Call("brightness")).GetInt32()); Assert.Equal(3,(await Call("calibratorstate")).GetInt32());
            await Call("calibratoron","Brightness","17");
            Assert.True(device.CalibratorChanging); Assert.Equal(17,device.Brightness);
            publisher.Kill(); await publisher.WaitForExitAsync();
            await Eventually(async () => (await host.Command(new {op="sourceStatus",source=source.SourceId})).GetProperty("leaseCount").GetInt32()==1);
            Assert.True(device.Connected); Assert.True(device.LightOn);
            Assert.Equal(new[]{"calibratoron","calibratoron"},source.PanelCommands.ToArray());
        } catch (Exception error) { primaryFailure=error; throw; }
        finally {
            if (!publisher.HasExited) { publisher.Kill(); await publisher.WaitForExitAsync(); }
            try {
                await Task.WhenAll(output,errors);
                if (primaryFailure is not null) { Console.WriteLine(await output); Console.WriteLine(await errors); }
            } catch (OperationCanceledException) when (primaryFailure is not null) { Console.WriteLine("Panel publisher output capture exceeded its deadline; retaining original failure"); }
        }
    }
    [Theory]
    [InlineData(1)]
    [InlineData(2)]
    public async Task NativePanelSharesActualCoverEndpointsAndLogicalZeroOn(int version)
    {
        using var source = new HubCoverCalibratorServer(version);
        await using var host = await Host.Open(config => source.AddTo(config,4,17));
        using var first = new HubCoverCalibratorDevice(host.Selection(3,"covercalibrator"),host.Executable,host.Workers);
        using var sibling = new HubCoverCalibratorDevice(host.Selection(4,"covercalibrator"),host.Executable,host.Workers);
        Assert.False(first.Connected); Assert.False(first.SupportsOpenClose); Assert.False(first.SupportsOnOff);
        Assert.Empty(source.PanelCommands);
        await first.Connect(CancellationToken.None); await sibling.Connect(CancellationToken.None);
        Assert.True(first.SupportsOpenClose); Assert.True(first.SupportsOnOff);
        Assert.Equal(4096,first.MaxBrightness); Assert.Equal(0,first.MinBrightness);
        Assert.Equal(CoverState.Closed,first.CoverState);
        var opened = first.Open(CancellationToken.None,10);
        await Eventually(() => Task.FromResult(source.PanelCommands.Count == 1));
        Assert.False(opened.IsCompleted); Assert.True(sibling.CoverMoving);
        Assert.Equal(CoverState.NeitherOpenNorClosed,sibling.CoverState);
        source.Values["coverstate"] = 3; source.Values["covermoving"] = false;
        Assert.True(await opened); Assert.Equal(CoverState.Open,sibling.CoverState);
        var close = sibling.Close(CancellationToken.None,10);
        await Eventually(() => Task.FromResult(source.PanelCommands.Count == 2));
        source.Values["coverstate"] = 1; source.Values["covermoving"] = false;
        Assert.True(await close);
        var light = Task.Run(() => first.Brightness = 0);
        await Eventually(() => Task.FromResult(source.PanelCommands.Count == 3));
        Assert.False(light.IsCompleted); Assert.True(sibling.CalibratorChanging);
        source.Values["calibratorstate"] = 3; source.Values["calibratorchanging"] = false;
        await light;
        Assert.Equal(0,sibling.Brightness); Assert.True(sibling.LightOn);
        first.LightOn = false; Assert.False(sibling.LightOn);
        var restored = Task.Run(() => first.LightOn = true);
        await Eventually(() => Task.FromResult(source.PanelCommands.Count == 5));
        Assert.Equal(0,source.Values["brightness"]);
        source.Values["calibratorstate"] = 3; source.Values["calibratorchanging"] = false;
        await restored;
        first.Disconnect(); Assert.True(sibling.Connected); Assert.True(sibling.LightOn);
        Assert.Equal(1,(await host.Command(new {op="sourceStatus",source=source.SourceId})).GetProperty("leaseCount").GetInt32());
        sibling.Disconnect();
        Assert.Equal(new[]{"opencover","closecover","calibratoron","calibratoroff","calibratoron"},source.PanelCommands.ToArray());
    }
    [Fact]
    public async Task NativePanelWaitFailureAndCancellationNeverInventCompensatingCommands()
    {
        using var source = new HubCoverCalibratorServer();
        await using var host = await Host.Open(config => source.AddTo(config,4,17));
        using var first = new HubCoverCalibratorDevice(host.Selection(3,"covercalibrator"),host.Executable,host.Workers);
        using var sibling = new HubCoverCalibratorDevice(host.Selection(4,"covercalibrator"),host.Executable,host.Workers);
        await first.Connect(CancellationToken.None); await sibling.Connect(CancellationToken.None);
        using var canceled = new CancellationTokenSource(); canceled.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => first.Open(canceled.Token));
        await Assert.ThrowsAsync<ArgumentOutOfRangeException>(() => first.Open(CancellationToken.None,0));
        Assert.Empty(source.PanelCommands);
        var stopped = first.Open(CancellationToken.None,10);
        await Eventually(() => Task.FromResult(source.PanelCommands.Count == 1));
        source.Values["coverstate"] = 4; source.Values["covermoving"] = false;
        await Assert.ThrowsAsync<IOException>(() => stopped);
        Assert.Equal(CoverState.Unknown,sibling.CoverState); Assert.False(sibling.CoverMoving);
        var changedLight = Task.Run(() => first.Brightness = 10);
        await Eventually(() => Task.FromResult(source.PanelCommands.Count == 2));
        source.Values["brightness"] = 20; source.Values["calibratorstate"] = 3; source.Values["calibratorchanging"] = false;
        await Assert.ThrowsAsync<IOException>(() => changedLight);
        using var token = new CancellationTokenSource();
        var moving = first.Close(token.Token,100);
        await Eventually(() => Task.FromResult(source.PanelCommands.Count == 3));
        token.Cancel(); await Assert.ThrowsAnyAsync<OperationCanceledException>(() => moving);
        Assert.Equal(new[]{"opencover","calibratoron","closecover"},source.PanelCommands.ToArray());
        Assert.True(sibling.Connected);
        first.Disconnect(); sibling.Disconnect();
        Assert.Equal(3,source.PanelCommands.Count);
    }
    [Theory]
    [InlineData(1)]
    [InlineData(2)]
    public async Task NativePanelLostOnReplyFencesSiblingsWithoutReplayOrOff(int version)
    {
        using var source = new HubCoverCalibratorServer(version) { LoseMoveReply = true };
        await using var host = await Host.Open(config => source.AddTo(config,4,17));
        using var first = new HubCoverCalibratorDevice(host.Selection(3,"covercalibrator"),host.Executable,host.Workers);
        using var sibling = new HubCoverCalibratorDevice(host.Selection(4,"covercalibrator"),host.Executable,host.Workers);
        await first.Connect(CancellationToken.None); await sibling.Connect(CancellationToken.None);
        var lost = Assert.Throws<HubException>(() => first.Brightness = 17);
        await AssertAccessoryFailure(host,source,lost,"uncertain","panel lost On reply");
        var off = Assert.Throws<HubException>(() => sibling.LightOn = false);
        await AssertAccessoryFailure(host,source,off,"uncertain","panel sibling Off fence");
        Assert.Throws<HubException>(() => sibling.HaltCover());
        Assert.False(first.Connected); Assert.False(sibling.Connected);
        Assert.Equal(new[]{"calibratoron"},source.PanelCommands.ToArray());
        Assert.Equal(17,source.Values["brightness"]);
        Assert.True((await host.Command(new {op="sourceStatus",source=source.SourceId})).GetProperty("writeUncertain").GetBoolean());
    }
    [Fact]
    public async Task NativePanelPreservesAbsenceLiveLimitsAndModernErrors()
    {
        using var source = new HubCoverCalibratorServer();
        source.Values["calibratorstate"] = 0; source.Values.TryRemove("brightness",out _); source.Values.TryRemove("maxbrightness",out _);
        await using var host = await Host.Open(config => source.AddTo(config,4));
        using var device = new HubCoverCalibratorDevice(host.Selection(3,"covercalibrator"),host.Executable,host.Workers);
        await device.Connect(CancellationToken.None);
        Assert.True(device.SupportsOpenClose); Assert.False(device.SupportsOnOff);
        Assert.Equal(0,device.MaxBrightness); Assert.Equal(0,device.Brightness); Assert.False(device.LightOn);
        Assert.Throws<HubException>(() => device.LightOn = true);
        Assert.Empty(source.PanelCommands);
        source.Values["calibratorstate"] = 1; source.Values["brightness"] = 0; source.Values["maxbrightness"] = 17;
        Assert.True(device.SupportsOnOff); Assert.Equal(17,device.MaxBrightness);
        Assert.Throws<ArgumentOutOfRangeException>(() => device.Brightness = -1);
        Assert.Throws<HubException>(() => device.Brightness = 18);
        source.Values["covermoving"] = 0;
        Assert.Throws<HubException>(() => _ = device.CoverMoving);
        source.Values["covermoving"] = false; source.Values["coverstate"] = 0;
        Assert.False(device.SupportsOpenClose); Assert.Equal(CoverState.NotPresent,device.CoverState);
        source.Values["calibratorstate"] = 4;
        Assert.Throws<InvalidOperationException>(() => _ = device.LightOn);
        Assert.Empty(source.PanelCommands);
        Assert.True(device.Connected);
    }
    [Fact]
    public void NativePanelChoicesAndWireBoundsAreInert()
    {
        var directory = Path.Combine(Path.GetTempPath(),"Regain panel choices "+Guid.NewGuid().ToString("N")); Directory.CreateDirectory(directory);
        try {
            var store = new HubSelectionStore(Path.Combine(directory,"bindings.json"));
            var binding = Binding(directory,"covercalibrator"); store.Save(binding,Guid.Empty);
            var choices = HubEquipment.Choices<IFlatDevice>("covercalibrator",selection => new HubCoverCalibratorDevice(selection),store);
            Assert.Equal(binding.Id,choices[0].Id); Assert.EndsWith("Configure.covercalibrator",choices[1].Id);
            foreach (var choice in choices) { Assert.False(choice.Connected); ((IDisposable)choice).Dispose(); }
            foreach (var (property,json) in new[]{(HubCoverCalibratorProperty.Brightness,"-1"),(HubCoverCalibratorProperty.Brightness,"2147483648"),(HubCoverCalibratorProperty.MaxBrightness,"0"),(HubCoverCalibratorProperty.CoverState,"6"),(HubCoverCalibratorProperty.CalibratorState,"1.5"),(HubCoverCalibratorProperty.CoverMoving,"0"),(HubCoverCalibratorProperty.CalibratorChanging,"\"false\"")}) {
                using var value = JsonDocument.Parse(json);
                Assert.Throws<HubException>(() => HubCoverCalibratorProtocol.Validate(property,value.RootElement));
            }
            Assert.Throws<ArgumentOutOfRangeException>(() => HubCoverCalibratorProtocol.On(-1));
            Assert.Throws<HubException>(() => HubCoverCalibratorProtocol.StateProperty("MaxBrightness"));
        } finally { Directory.Delete(directory,true); }
    }
}
