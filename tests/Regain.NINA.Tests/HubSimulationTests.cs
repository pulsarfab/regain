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
    public async Task NativeRotatorSimulationSharesDescriptorsCoordinatesAndCompletesNinaMoves()
    {
        Guid source = default;
        await using var host = await Host.Open(config => source = HubRotatorSimulation.AddTo(config,4,7));
        using var editor = await Editor(host); await editor.ReloadAsync();
        var controls = editor.SimulationControls(source); Assert.Equal(12,controls.Count);
        var angle = controls.Single(control => control.Path.SequenceEqual(new[] {"rotator","position"}));
        foreach (var value in new[] {"360","359.9999999","-1","1e39"}) Assert.Throws<InvalidOperationException>(()=>angle.Parse(value));
        var step = controls.Single(control => control.Path.SequenceEqual(new[] {"rotator","stepSize"}));
        Assert.Throws<InvalidOperationException>(()=>step.Parse("1e-300"));
        await editor.UpdateSimulationAsync(source, JsonSerializer.SerializeToElement(new {rotator=new {
            position=20.0,mechanicalPosition=350.0,targetPosition=20.0}}));
        await Eventually(async ()=>(await host.Status(3)).GetProperty("leaseCount").GetInt32()==0);
        using var first = new HubRotatorDevice(host.Selection(3,"rotator"),host.Executable,host.Workers);
        using var second = new HubRotatorDevice(host.Selection(4,"rotator"),host.Executable,host.Workers);
        await first.Connect(CancellationToken.None); await second.Connect(CancellationToken.None);
        Assert.Equal(20f,first.Position); Assert.Equal(350f,second.MechanicalPosition);
        first.Sync(42.5f); Assert.True(first.Synced); Assert.False(second.Synced);
        Assert.Equal(42.5f,second.Position); Assert.Equal(350f,second.MechanicalPosition);
        second.Reverse=true; Assert.True(first.Reverse);
        Assert.True(await first.Move(-721.5f,CancellationToken.None));
        Assert.Equal(41f,second.Position); Assert.Equal(348.5f,second.MechanicalPosition);
        Assert.True(await first.MoveAbsolute(50f,CancellationToken.None));
        Assert.Equal(357.5f,second.MechanicalPosition);
        Assert.True(await second.MoveAbsoluteMechanical(12.25f,CancellationToken.None));
        Assert.Equal(64.75f,first.Position);
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new {rotator=new {stepSizeAvailable=false}}));
        Assert.True(float.IsNaN(first.StepSize)); Assert.True(second.Connected);
        var status=(await editor.SourceStatusAsync(source)).GetProperty("simulation");
        var malformed=JsonNode.Parse(status.GetRawText())!.AsObject(); malformed["rotator"]!["position"]=359.9999999;
        Assert.Throws<InvalidOperationException>(()=>editor.ValidateSimulationStatus(source,JsonSerializer.SerializeToElement(malformed)));
        malformed=JsonNode.Parse(status.GetRawText())!.AsObject(); malformed["rotator"]!["extra"]=true;
        Assert.Throws<HubException>(()=>editor.ValidateSimulationStatus(source,JsonSerializer.SerializeToElement(malformed)));
        first.Disconnect(); Assert.True(second.Connected); Assert.Equal(64.75f,second.Position);
        second.Disconnect(); await Eventually(async ()=>(await host.Status(3)).GetProperty("leaseCount").GetInt32()==0);
    }
    [Fact]
    public async Task NativeRotatorSimulationCancellationStoppedShortAndUncertaintyRemainExplicit()
    {
        await using var host=await Host.Open(config=>HubRotatorSimulation.AddTo(config,4,7));
        using var first=new HubRotatorDevice(host.Selection(3,"rotator"),host.Executable,host.Workers);
        using var second=new HubRotatorDevice(host.Selection(4,"rotator"),host.Executable,host.Workers);
        await first.Connect(CancellationToken.None); await second.Connect(CancellationToken.None);
        await host.Update(3,new {fault="stalledMotion"});
        using var cancellation=new CancellationTokenSource();
        var move=first.MoveAbsolute(10f,cancellation.Token);
        await Eventually(async ()=>(await host.Status(3)).GetProperty("simulation").GetProperty("rotator").GetProperty("isMoving").GetBoolean());
        cancellation.Cancel(); await Assert.ThrowsAnyAsync<OperationCanceledException>(()=>move);
        first.Disconnect(); Assert.True(second.IsMoving);
        await host.Update(3,new {fault="stoppedShort",rotator=new {position=0.0,mechanicalPosition=0.0,isMoving=false}});
        await Assert.ThrowsAsync<IOException>(()=>second.MoveAbsolute(0.5f,CancellationToken.None));
        Assert.Equal(359.5f,second.Position); Assert.False(second.IsMoving);
        await host.Update(3,new {fault="uncertainWrite"});
        Assert.Equal("uncertain",(await Assert.ThrowsAsync<HubException>(()=>second.Move(5f,CancellationToken.None))).Remote!.Code);
        await host.Update(3,new {fault="none"});
        Assert.True((await host.Status(3)).GetProperty("writeUncertain").GetBoolean());
        Assert.Equal("uncertain",Assert.Throws<HubException>(()=>second.Halt()).Remote!.Code);
        await Eventually(async ()=>(await host.Status(3)).GetProperty("simulation").GetProperty("rotator").GetProperty("position").GetDouble()==4.5);
    }
    [Fact]
    public async Task SharedNativeRotatorSimulationWindowUsesHostControlsAndSparseUpdates()
    {
        await Wpf(async ()=>{
            await using var host=await Host.Open(config=>HubRotatorSimulation.AddTo(config,4));
            var window=new HubConfigurationWindow(host.Executable,host.ConfigPath,host.Selection(0,"switch").InstanceId);
            try {
                window.Show(); var review=Controls<Button>(window).Single(button=>(string)button.Content=="Review changes");
                await UiUntil(()=>review.IsEnabled); Controls<TabControl>(window).Single().SelectedIndex=4;
                var sources=Controls<ComboBox>(window).Single(combo=>(string?)combo.Tag=="simulation-source");
                sources.SelectedItem=sources.Items.OfType<ComboBoxItem>().Single(item=>(string)item.Content=="Explicit simulation rotator");
                var include=Controls<CheckBox>(window).Single(box=>(string?)box.Tag=="simulation-change-rotator-position");
                include.IsChecked=true; include.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                var value=Controls<TextBox>(window).Single(box=>(string?)box.Tag=="simulation-value-rotator-position"); value.Text="42.5";
                await window.Dispatcher.InvokeAsync(()=>{},System.Windows.Threading.DispatcherPriority.ContextIdle);
                window.UpdateLayout(); value.BringIntoView(); await Capture(window,"hub-native-rotator-simulation.png");
                var apply=Controls<Button>(window).Single(button=>(string)button.Content=="Apply selected simulation changes");
                apply.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(()=>review.IsEnabled && !apply.IsEnabled);
                var state=(await host.Status(3)).GetProperty("simulation").GetProperty("rotator");
                Assert.Equal(42.5,state.GetProperty("position").GetDouble()); Assert.Equal(0,state.GetProperty("mechanicalPosition").GetDouble());
                await Eventually(async ()=>(await host.Status(3)).GetProperty("leaseCount").GetInt32()==0);
            } finally {window.Close();}
        });
    }
    [Fact]
    public async Task NativeFocuserSimulationUsesSharedIntegerControlsAndCompletesNinaMoves()
    {
        Guid source = default;
        await using var host = await Host.Open(config => source = HubFocuserSimulation.AddTo(config,4,7));
        using var editor = await Editor(host); await editor.ReloadAsync();
        var controls = editor.SimulationControls(source); Assert.Equal(15,controls.Count);
        var position = controls.Single(control => control.Path.SequenceEqual(new[]{"focuser","position"}));
        Assert.Equal("integer",position.Type); Assert.Equal(100000,position.Parse("100000").GetInt32());
        foreach (var invalid in new[]{"1.5","2147483648","-1"}) Assert.Throws<InvalidOperationException>(()=>position.Parse(invalid));
        var revision = editor.Draft!.Revision;
        await editor.UpdateSimulationAsync(source,HubEditorSession.SimulationPatch([new(position,position.Parse("100000"))]));
        using var first = new HubFocuserDevice(host.Selection(3,"focuser"),host.Executable,host.Workers);
        using var second = new HubFocuserDevice(host.Selection(4,"focuser"),host.Executable,host.Workers);
        await first.Connect(CancellationToken.None); await second.Connect(CancellationToken.None);
        Assert.Equal(100000,first.Position);
        await first.Move(99900,CancellationToken.None,0);
        Assert.Equal(99900,second.Position); Assert.False(second.IsMoving);
        await editor.UpdateSimulationAsync(source,JsonSerializer.SerializeToElement(new { focuser = new { temperatureAvailable = false, stepSizeAvailable = false } }));
        Assert.True(double.IsNaN(first.Temperature)); Assert.True(double.IsNaN(first.StepSize));
        Assert.Equal(revision,(await host.Command(new {op="getConfig"})).GetProperty("revision").GetGuid());
        var status = await editor.SourceStatusAsync(source); Assert.True(status.GetProperty("simulated").GetBoolean());
        await Eventually(async () => (await host.Status(3)).GetProperty("leaseCount").GetInt32()==2);
        var malformed = JsonNode.Parse(status.GetProperty("simulation").GetRawText())!;
        malformed["focuser"]!["extra"] = true;
        Assert.Throws<HubException>(()=>editor.ValidateSimulationStatus(source,JsonSerializer.SerializeToElement(malformed)));
        first.Disconnect(); second.Disconnect();
        await Eventually(async () => (await host.Status(3)).GetProperty("leaseCount").GetInt32()==0);
    }
    [Fact]
    public async Task NativeFocuserSimulationWindowUsesIntegerFieldsAndSparseUpdates()
    {
        await Wpf(async () => {
            await using var host = await Host.Open(config => HubFocuserSimulation.AddTo(config,4));
            var window = new HubConfigurationWindow(host.Executable,host.ConfigPath,host.Selection(0,"switch").InstanceId);
            try {
                window.Show(); var review=Controls<Button>(window).Single(button=>(string)button.Content=="Review changes");
                await UiUntil(()=>review.IsEnabled); Controls<TabControl>(window).Single().SelectedIndex=4;
                var sources=Controls<ComboBox>(window).Single(combo=>(string?)combo.Tag=="simulation-source");
                sources.SelectedItem=sources.Items.OfType<ComboBoxItem>().Single(item=>(string)item.Content=="Explicit simulation focuser");
                var include=Controls<CheckBox>(window).Single(box=>(string?)box.Tag=="simulation-change-focuser-position");
                include.IsChecked=true; include.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                var position=Controls<TextBox>(window).Single(box=>(string?)box.Tag=="simulation-value-focuser-position");
                position.Text="50100";
                await window.Dispatcher.InvokeAsync(()=>{},System.Windows.Threading.DispatcherPriority.ContextIdle);
                window.UpdateLayout(); position.BringIntoView();
                await Capture(window,"hub-native-focuser-simulation.png");
                var apply=Controls<Button>(window).Single(button=>(string)button.Content=="Apply selected simulation changes");
                apply.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(()=>review.IsEnabled && !apply.IsEnabled);
                var state=(await host.Status(3)).GetProperty("simulation").GetProperty("focuser");
                Assert.Equal(50100,state.GetProperty("position").GetInt32()); Assert.Equal(12,state.GetProperty("temperature").GetDouble());
                Assert.False(state.GetProperty("isMoving").GetBoolean());
                await Eventually(async ()=>(await host.Status(3)).GetProperty("leaseCount").GetInt32()==0);
            } finally { window.Close(); }
        });
    }
    [Fact]
    public async Task NativeFocuserSimulationStallCancellationAndStoppedShortRemainExplicit()
    {
        await using var host = await Host.Open(config => HubFocuserSimulation.AddTo(config,4));
        using var focuser = new HubFocuserDevice(host.Selection(3,"focuser"),host.Executable,host.Workers);
        await focuser.Connect(CancellationToken.None);
        await host.Update(3,new {fault="stalledMotion"});
        using var cancellation = new CancellationTokenSource();
        var move = focuser.Move(50100,cancellation.Token,0);
        await Eventually(async () => (await host.Status(3)).GetProperty("simulation").GetProperty("focuser").GetProperty("isMoving").GetBoolean());
        cancellation.Cancel(); await Assert.ThrowsAnyAsync<OperationCanceledException>(()=>move);
        focuser.Disconnect();
        await Eventually(async () => (await host.Status(3)).GetProperty("leaseCount").GetInt32()==0);
        Assert.True((await host.Status(3)).GetProperty("simulation").GetProperty("focuser").GetProperty("isMoving").GetBoolean());
        await host.Update(3,new {fault="stoppedShort",focuser=new {position=50000,isMoving=false}});
        await focuser.Connect(CancellationToken.None);
        await Assert.ThrowsAsync<IOException>(()=>focuser.Move(50100,CancellationToken.None,0));
        Assert.Equal(50099,focuser.Position); Assert.False(focuser.IsMoving);
    }
    [Fact]
    public async Task NativeFocuserSimulationFaultClearingCannotReplayAnUncertainMove()
    {
        await using var host = await Host.Open(config => HubFocuserSimulation.AddTo(config,4,7));
        using var first = new HubFocuserDevice(host.Selection(3,"focuser"),host.Executable,host.Workers);
        using var second = new HubFocuserDevice(host.Selection(4,"focuser"),host.Executable,host.Workers);
        await first.Connect(CancellationToken.None); await second.Connect(CancellationToken.None);
        await host.Update(3,new {fault="uncertainWrite"});
        Assert.Equal("uncertain",(await Assert.ThrowsAsync<HubException>(()=>first.Move(50100,CancellationToken.None,0))).Remote!.Code);
        await host.Update(3,new {fault="none"});
        Assert.True((await host.Status(3)).GetProperty("writeUncertain").GetBoolean());
        Assert.Equal("uncertain",(await Assert.ThrowsAsync<HubException>(()=>second.Move(50200,CancellationToken.None,0))).Remote!.Code);
        Assert.False(first.Connected); Assert.False(second.Connected);
        await Eventually(async () => (await host.Status(3)).GetProperty("simulation").GetProperty("focuser").GetProperty("position").GetInt32()==50100);
    }
    [Fact]
    public async Task NativeSimulationControlsPatchSharedStateAndPreserveConfigurationAndLeases()
    {
        await using var host = await Host.Open(); using var editor = await Editor(host); await editor.ReloadAsync();
        var revision = editor.Draft!.Revision;
        Guid Source(int i) => Guid.Parse(host.Config["sources"]![i]!["id"]!.GetValue<string>());
        using var device = host.Switch(); await device.Connect(CancellationToken.None);
        var controls = editor.SimulationControls(Source(0)); Assert.Equal(5, controls.Count);
        Assert.True(await editor.ReviewAsync());
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.UpdateSimulationAsync(Source(0), JsonSerializer.SerializeToElement(new { safe = true })));
        Assert.Equal(HubEditorState.Reviewed, editor.State);
        var channel = controls.Single(c => c.Path.SequenceEqual(new[] { "switchValues", "1" }));
        var update = HubEditorSession.SimulationPatch([new(channel, channel.Parse("42.2"))]);
        var result = await editor.UpdateSimulationAsync(Source(0), update);
        Assert.Equal(42, result.GetProperty("simulation").GetProperty("switchValues").GetProperty("1").GetDouble()); // backend rounds to supported step
        Assert.Equal(12, result.GetProperty("simulation").GetProperty("switchValues").GetProperty("2").GetDouble());
        Assert.Equal(HubEditorState.Editing, editor.State); Assert.True(device.Connected);
        await Eventually(async () => (await host.Status(0)).GetProperty("leaseCount").GetInt32() == 1);
        Assert.Equal("simulationUpdate", editor.DiagnosticSnapshot().GetProperty("observation").GetProperty("kind").GetString());
        Assert.Equal(2, editor.SimulationControls(Source(1)).Count);
        await editor.UpdateSimulationAsync(Source(1), JsonSerializer.SerializeToElement(new { safe = true, fault = "none" }));
        Assert.True((await host.Status(1)).GetProperty("simulation").GetProperty("safe").GetBoolean());
        var weather = editor.SimulationControls(Source(2)); Assert.Equal(15, weather.Count);
        Assert.Throws<InvalidOperationException>(() => weather.Single(c => c.Path.SequenceEqual(new[] { "weather", "pressure" })).Parse("0"));
        var temperature = weather.Single(c => c.Path.SequenceEqual(new[] { "weather", "temperature" }));
        await editor.UpdateSimulationAsync(Source(2), HubEditorSession.SimulationPatch([new(temperature, temperature.Parse("", true))]));
        Assert.Equal(JsonValueKind.Null, (await host.Status(2)).GetProperty("simulation").GetProperty("weather").GetProperty("temperature").ValueKind);
        Assert.Equal(revision, (await host.Command(new { op = "getConfig" })).GetProperty("revision").GetGuid());
        for (int i = 1; i < 3; i++) await Eventually(async () => (await host.Status(i)).GetProperty("leaseCount").GetInt32() == 0);
        device.Disconnect(); await Eventually(async () => (await host.Status(0)).GetProperty("leaseCount").GetInt32() == 0);
        var candidate = JsonNode.Parse((await host.Command(new { op = "getConfig" })).GetRawText())!;
        candidate["sources"]![0]!["label"] = "New revision of simulation";
        await host.Command(new { op = "applyConfig", expectedRevision = revision, candidate });
        var failure = await Assert.ThrowsAsync<HubException>(() => editor.UpdateSimulationAsync(Source(0), update));
        Assert.Equal("revisionConflict", failure.Remote!.Code); Assert.Equal(HubEditorState.Uncertain, editor.State);
        Assert.Equal(0, (await host.Status(0)).GetProperty("simulation").GetProperty("switchValues").GetProperty("1").GetDouble());
        await editor.ReloadAsync(); Assert.NotEqual(revision, editor.Draft.Revision);
    }
    [Theory]
    [InlineData("lost")]
    [InlineData("wrongRevision")]
    [InlineData("malformed")]
    [InlineData("responseTooLarge")]
    public async Task NativeSimulationLostOrInvalidReplyRequiresReloadAndReadWithoutRepeatingChanges(string fault)
    {
        await using var host = await Host.Open(); using var attached = await Editor(host); await attached.ReloadAsync();
        var source = host.Config["sources"]![0]!["id"]!.GetValue<string>(); int writes = 0;
        using var editor = new HubEditorSession(attached.InstanceId, async (command, token) => {
            if (command.GetProperty("op").GetString() != "updateSimulation") return await host.Client.RequestAsync(command, token);
            writes++; var result = await host.Client.RequestAsync(command, token);
            if (fault == "lost") throw new HubException(HubFailure.Uncertain);
            if (fault == "responseTooLarge") throw new HubException(HubFailure.Remote,new HubRemoteError(JsonSerializer.SerializeToElement(new {
                code="responseTooLarge",message="Hub response exceeds the frame limit",upstreamCode=(int?)null,retryAfterSeconds=(double?)null,fields=Array.Empty<object>()
            })));
            var reply = JsonNode.Parse(result.GetRawText())!;
            if (fault == "wrongRevision") reply["configurationRevision"] = Guid.NewGuid().ToString();
            if (fault == "malformed") reply["simulation"]!["fault"] = "not-a-fault";
            return JsonSerializer.SerializeToElement(reply);
        }, () => { });
        await editor.ReloadAsync(); var patch = JsonSerializer.SerializeToElement(new { switchValues = new Dictionary<string,double> { ["1"] = 9 } });
        await Assert.ThrowsAsync<HubException>(() => editor.UpdateSimulationAsync(Guid.Parse(source),patch));
        Assert.Equal(HubEditorState.Uncertain,editor.State); Assert.Null(editor.LastSourceObservation);
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.UpdateSimulationAsync(Guid.Parse(source),patch)); Assert.Equal(1,writes);
        await editor.ReloadAsync(); Assert.Equal(9,(await editor.SourceStatusAsync(Guid.Parse(source))).GetProperty("simulation").GetProperty("switchValues").GetProperty("1").GetDouble());
        Assert.Equal(1,writes);
    }
    [Fact]
    public async Task NativeSimulationWindowChangesOnlySelectedFieldsAndResetsItsForm()
    {
        await Wpf(async () => {
            await using var host = await Host.Open();
            var window = new HubConfigurationWindow(host.Executable,host.ConfigPath,host.Selection(0,"switch").InstanceId);
            try {
                window.Show(); var review = Controls<Button>(window).Single(b => (string)b.Content == "Review changes"); await UiUntil(() => review.IsEnabled);
                Controls<TabControl>(window).Single().SelectedIndex = 4;
                var apply = Controls<Button>(window).Single(b => (string)b.Content == "Apply selected simulation changes"); Assert.False(apply.IsEnabled);
                var include = Controls<CheckBox>(window).Single(c => (string?)c.Tag == "simulation-change-switchValues-1");
                include.IsChecked = true; include.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
                Controls<TextBox>(window).Single(c => (string?)c.Tag == "simulation-value-switchValues-1").Text = "17";
                await Capture(window,"hub-native-simulation-controls.png");
                apply.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(() => review.IsEnabled && !apply.IsEnabled);
                var status = await host.Status(0); Assert.Equal(17,status.GetProperty("simulation").GetProperty("switchValues").GetProperty("1").GetDouble());
                Assert.Equal(12,status.GetProperty("simulation").GetProperty("switchValues").GetProperty("2").GetDouble());
                Assert.False(Controls<CheckBox>(window).Single(c => (string?)c.Tag == "simulation-change-switchValues-1").IsChecked);
                var read = Controls<Button>(window).Single(b => (string)b.Content == "Read current simulation and reset form");
                await host.Update(0,new { switchValues = new Dictionary<string,double> { ["1"] = 27 } });
                read.RaiseEvent(new RoutedEventArgs(Button.ClickEvent)); await UiUntil(() => read.IsEnabled);
                Assert.Equal(27,double.Parse(Controls<TextBox>(window).Single(c => (string?)c.Tag == "simulation-value-switchValues-1").Text,System.Globalization.CultureInfo.InvariantCulture));
                for (int i=0;i<3;i++) await Eventually(async () => (await host.Status(i)).GetProperty("leaseCount").GetInt32()==0);
            } finally { window.Close(); }
        });
    }
}
