using System.Text.Json;
using System.Text.Json.Nodes;
using System.Windows;
using System.Windows.Controls;
using NINA.Equipment.Interfaces;
using Regain.Hub;
using Regain.TestFixtures;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    [Fact]
    public async Task NativePanelSimulationSharesDescriptorsIndependentCompletionAndLogicalZeroOn()
    {
        Guid source=default;
        await using var host=await Host.Open(config=>source=HubCoverCalibratorSimulation.AddTo(config,4,17));
        using var editor=await Editor(host); await editor.ReloadAsync();
        var controls=editor.SimulationControls(source); Assert.Equal(10,controls.Count);
        var brightness=controls.Single(c=>c.Path.SequenceEqual(new[]{"coverCalibrator","brightness"}));
        foreach(var value in new[]{"-1","2147483648","1.5","true"}) Assert.Throws<InvalidOperationException>(()=>brightness.Parse(value));
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new {fault="stalledMotion",coverCalibrator=new {moveDurationSeconds=300,lightDurationSeconds=1}}));
        await Eventually(async()=>(await host.Status(3)).GetProperty("leaseCount").GetInt32()==0);
        using var first=new HubCoverCalibratorDevice(host.Selection(3,"covercalibrator"),host.Executable,host.Workers);
        using var second=new HubCoverCalibratorDevice(host.Selection(4,"covercalibrator"),host.Executable,host.Workers);
        await first.Connect(CancellationToken.None); await second.Connect(CancellationToken.None);
        var opening=first.Open(CancellationToken.None,10);
        await Eventually(()=>Task.FromResult(second.CoverMoving));
        var light=Task.Run(()=>second.Brightness=0);
        await Eventually(()=>Task.FromResult(first.CalibratorChanging));
        Assert.True(first.CoverMoving); Assert.True(first.LightOn);
        await host.Update(3,new {fault="none"});
        await light; Assert.Equal(0,first.Brightness); Assert.True(first.LightOn);
        Assert.True(first.CoverMoving); Assert.False(first.CalibratorChanging);
        await host.Update(3,new {coverCalibrator=new {coverState=3,coverMoving=false}});
        Assert.True(await opening); Assert.Equal(CoverState.Open,second.CoverState);
        var state=(await editor.SourceStatusAsync(source)).GetProperty("simulation");
        foreach(var change in new Action<JsonObject>[] {
            p=>p["brightness"]=4097, p=>{p["calibratorState"]=1;p["brightness"]=1;},
            p=>{p["coverState"]=0;p["coverMoving"]=true;},
            p=>{p["calibratorState"]=0;p["calibratorChanging"]=true;}}) {
            var bad=JsonNode.Parse(state.GetRawText())!.AsObject(); change(bad["coverCalibrator"]!.AsObject());
            Assert.Throws<HubException>(()=>editor.ValidateSimulationStatus(source,JsonSerializer.SerializeToElement(bad)));
        }
        first.Disconnect(); Assert.True(second.Connected); Assert.True(second.LightOn);
        second.Disconnect(); await Eventually(async()=>(await host.Status(3)).GetProperty("leaseCount").GetInt32()==0);
        Assert.Equal(3,(await host.Status(3)).GetProperty("simulation").GetProperty("coverCalibrator").GetProperty("calibratorState").GetInt32());
    }

    [Fact]
    public async Task NativePanelSimulationCancellationStoppedShortAndUncertaintyNeverActuateCleanup()
    {
        await using var host=await Host.Open(config=>HubCoverCalibratorSimulation.AddTo(config,4,17));
        using var first=new HubCoverCalibratorDevice(host.Selection(3,"covercalibrator"),host.Executable,host.Workers);
        using var second=new HubCoverCalibratorDevice(host.Selection(4,"covercalibrator"),host.Executable,host.Workers);
        await first.Connect(CancellationToken.None); await second.Connect(CancellationToken.None);
        await host.Update(3,new {fault="stalledMotion"});
        using var cancellation=new CancellationTokenSource(); var opening=first.Open(cancellation.Token,10);
        await Eventually(()=>Task.FromResult(second.CoverMoving)); cancellation.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(()=>opening);
        first.Disconnect(); Assert.True(second.CoverMoving); second.HaltCover();
        Assert.Equal(CoverState.Unknown,second.CoverState); Assert.False(second.CoverMoving);
        await host.Update(3,new {fault="stoppedShort"});
        await Assert.ThrowsAsync<IOException>(()=>second.Open(CancellationToken.None,10));
        Assert.Equal(CoverState.Unknown,second.CoverState);
        await Assert.ThrowsAsync<IOException>(()=>Task.Run(()=>second.Brightness=0));
        Assert.Equal(1,second.Brightness); Assert.False(second.CalibratorChanging);
        await host.Update(3,new {fault="uncertainWrite"});
        Assert.Equal("uncertain",(await Assert.ThrowsAsync<HubException>(()=>Task.Run(()=>second.Brightness=17))).Remote!.Code);
        await host.Update(3,new {fault="none"});
        Assert.Equal("uncertain",Assert.Throws<HubException>(()=>second.LightOn=false).Remote!.Code);
        Assert.Equal("uncertain",Assert.Throws<HubException>(()=>second.HaltCover()).Remote!.Code);
        await Eventually(async()=>(await host.Status(3)).GetProperty("simulation").GetProperty("coverCalibrator").GetProperty("brightness").GetInt32()==17);
        Assert.True((await host.Status(3)).GetProperty("writeUncertain").GetBoolean());
    }

    [Fact]
    public async Task SharedNativePanelSimulationWindowAppliesOnlySelectedComponentFields()
    {
        await Wpf(async()=>{
            await using var host=await Host.Open(config=>HubCoverCalibratorSimulation.AddTo(config,4));
            var window=new HubConfigurationWindow(host.Executable,host.ConfigPath,host.Selection(0,"switch").InstanceId);
            try {
                window.Show(); var review=Controls<Button>(window).Single(b=>(string)b.Content=="Review changes");
                await UiUntil(()=>review.IsEnabled); Controls<TabControl>(window).Single().SelectedIndex=4;
                var sources=Controls<ComboBox>(window).Single(c=>(string?)c.Tag=="simulation-source");
                sources.SelectedItem=sources.Items.OfType<ComboBoxItem>().Single(i=>(string)i.Content=="Explicit simulation covercalibrator");
                var include=Controls<CheckBox>(window).Single(c=>(string?)c.Tag=="simulation-change-coverCalibrator-coverState");
                include.IsChecked=true; include.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                var value=Controls<TextBox>(window).Single(c=>(string?)c.Tag=="simulation-value-coverCalibrator-coverState"); value.Text="4";
                await window.Dispatcher.InvokeAsync(()=>{},System.Windows.Threading.DispatcherPriority.ContextIdle);
                window.UpdateLayout(); value.BringIntoView(); await Capture(window,"hub-native-panel-simulation.png");
                var apply=Controls<Button>(window).Single(b=>(string)b.Content=="Apply selected simulation changes");
                apply.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(()=>review.IsEnabled && !apply.IsEnabled);
                var state=(await host.Status(3)).GetProperty("simulation").GetProperty("coverCalibrator");
                Assert.Equal(4,state.GetProperty("coverState").GetInt32()); Assert.False(state.GetProperty("coverMoving").GetBoolean());
                Assert.Equal(1,state.GetProperty("calibratorState").GetInt32()); Assert.Equal(0,state.GetProperty("brightness").GetInt32());
                await Eventually(async()=>(await host.Status(3)).GetProperty("leaseCount").GetInt32()==0);
            } finally { window.Close(); }
        });
    }
}
