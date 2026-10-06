using System.Text.Json;
using System.Text.Json.Nodes;
using System.Windows;
using System.Windows.Controls;
using Regain.Hub;
using Regain.TestFixtures;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    [Fact]
    public async Task NativeWheelSimulationSharesBoundedMetadataMotionAndUncertainty()
    {
        await Task.Run(async () => {
            Guid source=default;
            await using var host=await Host.Open(config=>source=HubFilterWheelSimulation.AddTo(config,4,17));
            using var editor=await Editor(host); await editor.ReloadAsync();
            var controls=editor.SimulationControls(source); Assert.Equal(6,controls.Count);
            var names=controls.Single(c=>c.Path.SequenceEqual(new[]{"filterWheel","names"}));
            var offsets=controls.Single(c=>c.Path.SequenceEqual(new[]{"filterWheel","focusOffsets"}));
            foreach(var value in new[]{"[]","[0]","{}","null","[\"\\ud800\"]"}) Assert.Throws<InvalidOperationException>(()=>names.Parse(value));
            foreach(var value in new[]{"[]","[1]","[\"0\"]","[0,2147483648]","[0,1.5]"}) Assert.Throws<InvalidOperationException>(()=>offsets.Parse(value));
            Assert.Equal(new[]{"L","Hα","😀",""},names.Parse("[\"L\",\"Hα\",\"😀\",\"\"]").EnumerateArray().Select(item=>item.GetString()));
            Assert.Equal(new[]{int.MinValue,0,int.MaxValue},offsets.Parse("[-2147483648,0,2147483647]").EnumerateArray().Select(item=>item.GetInt32()));
            await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new{filterWheel=new{names=new[]{"L","Hα",""},focusOffsets=new[]{-12,0,17}}}));
            await Eventually(async()=>(await host.Status(3)).GetProperty("leaseCount").GetInt32()==0);
            var (profiles,filters)=WheelProfile(); var original=filters[0];
            using var first=new HubFilterWheelDevice(profiles,host.Selection(3,"filterwheel"),host.Executable,host.Workers);
            using var second=new HubFilterWheelDevice(profiles,host.Selection(4,"filterwheel"),host.Executable,host.Workers);
            await first.Connect(CancellationToken.None); await second.Connect(CancellationToken.None);
            Assert.Equal(new[]{"L","Hα",""},first.Names); Assert.Equal(new[]{-12,0,17},second.FocusOffsets); Assert.Same(original,filters[0]);
            await host.Update(3,new{fault="stalledMotion"}); first.Position=2; Assert.Equal(-1,second.Position);
            Assert.Equal("busy",Assert.Throws<HubException>(()=>second.Position=1).Remote!.Code);
            first.Disconnect(); Assert.True(second.Connected); Assert.Equal(-1,second.Position);
            await host.Update(3,new{fault="none"}); await Eventually(()=>Task.FromResult(second.Position==2));
            var state=(await editor.SourceStatusAsync(source)).GetProperty("simulation");
            foreach(var mismatch in new[]{true,false}) {
                var malformed=JsonNode.Parse(state.GetRawText())!.AsObject();
                if(mismatch) malformed["filterWheel"]!["focusOffsets"]=JsonSerializer.SerializeToNode(new[]{0});
                else malformed["filterWheel"]!["position"]=3;
                Assert.Throws<HubException>(()=>editor.ValidateSimulationStatus(source,JsonSerializer.SerializeToElement(malformed)));
            }
            await first.Connect(CancellationToken.None); await host.Update(3,new{fault="uncertainWrite"});
            Assert.Equal("uncertain",Assert.Throws<HubException>(()=>first.Position=1).Remote!.Code);
            await host.Update(3,new{fault="none"});
            Assert.Equal("uncertain",Assert.Throws<HubException>(()=>second.Position=0).Remote!.Code);
            await Eventually(async()=>(await host.Status(3)).GetProperty("simulation").GetProperty("filterWheel").GetProperty("position").GetInt32()==1);
            Assert.True((await host.Status(3)).GetProperty("writeUncertain").GetBoolean());
        });
    }

    [Fact]
    public async Task SharedNativeWheelSimulationWindowAppliesOnlySelectedMetadata()
    {
        await Wpf(async()=>{
            await using var host=await Host.Open(config=>HubFilterWheelSimulation.AddTo(config,4));
            var window=new HubConfigurationWindow(host.Executable,host.ConfigPath,host.Selection(0,"switch").InstanceId);
            try {
                window.Show(); var review=Controls<Button>(window).Single(button=>(string)button.Content=="Review changes");
                await UiUntil(()=>review.IsEnabled); Controls<TabControl>(window).Single().SelectedIndex=4;
                var sources=Controls<ComboBox>(window).Single(combo=>(string?)combo.Tag=="simulation-source");
                sources.SelectedItem=sources.Items.OfType<ComboBoxItem>().Single(item=>(string)item.Content=="Explicit simulation filterwheel");
                var include=Controls<CheckBox>(window).Single(box=>(string?)box.Tag=="simulation-change-filterWheel-names");
                include.IsChecked=true; include.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                var value=Controls<TextBox>(window).Single(box=>(string?)box.Tag=="simulation-value-filterWheel-names");
                value.Text="[\"Lum\",\"Red\",\"Green\",\"Blue\",\"Hα\",\"OIII\",\"SII\"]";
                await window.Dispatcher.InvokeAsync(()=>{},System.Windows.Threading.DispatcherPriority.ContextIdle);
                window.UpdateLayout(); value.BringIntoView(); await Capture(window,"hub-native-wheel-simulation.png");
                var apply=Controls<Button>(window).Single(button=>(string)button.Content=="Apply selected simulation changes");
                apply.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(()=>review.IsEnabled && !apply.IsEnabled);
                var state=(await host.Status(3)).GetProperty("simulation").GetProperty("filterWheel");
                Assert.Equal("Lum",state.GetProperty("names")[0].GetString()); Assert.Equal(0,state.GetProperty("position").GetInt32());
                Assert.All(state.GetProperty("focusOffsets").EnumerateArray(),item=>Assert.Equal(0,item.GetInt32()));
                await Eventually(async()=>(await host.Status(3)).GetProperty("leaseCount").GetInt32()==0);
            } finally { window.Close(); }
        });
    }
}
