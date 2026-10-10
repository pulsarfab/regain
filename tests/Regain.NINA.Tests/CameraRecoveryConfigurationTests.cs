using System.Text.Json;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using Regain.Core;
using Regain.Rotator;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    [Fact]
    public async Task StandaloneRecoveryFormUsesHubMetadataAndKeepsHiddenLegacyValues()
    {
        await Wpf(async () => {
            var sections = new Dictionary<string,StackPanel>();
            var tabs=new TabControl();
            Panel Section(string title) {
                if(sections.TryGetValue(title,out var existing)) return existing;
                var body=new StackPanel {Margin=new Thickness(16)}; sections.Add(title,body);
                tabs.Items.Add(new TabItem {Header=title,Content=new ScrollViewer {Content=body,VerticalScrollBarVisibility=ScrollBarVisibility.Auto}});
                return body;
            }
            var values=JsonSerializer.SerializeToElement(new RecoveryOptions {UsbPortCycle=true},
                new JsonSerializerOptions {PropertyNamingPolicy=JsonNamingPolicy.CamelCase});
            var form=new CameraRecoveryForm(values,Section);
            Assert.Equal(14,form.Editors.Count);
            var reader=new HubConfiguration(JsonDocument.Parse(File.ReadAllText(Path.Combine(AppContext.BaseDirectory,"hub-config.json"))).RootElement);
            var schema=reader.Root.GetProperty("$defs").GetProperty("CameraRecovery");
            Assert.Equal(JsonSerializer.Serialize(schema),JsonSerializer.Serialize(CameraRecoveryConfiguration.Schema));
            TextBox Input(string key)=>form.Editors.OfType<TextBox>().Single(e=>AutomationProperties.GetAutomationId(e)=="recovery-"+key);
            Input("reconnectDelaySeconds").Text="0.000001";
            var changed=form.Read();
            Assert.Equal(.000001,changed.GetProperty("reconnectDelaySeconds").GetDouble());
            Assert.True(changed.GetProperty("usbPortCycle").GetBoolean());
            var typed=JsonSerializer.Deserialize<RecoveryOptions>(changed.GetRawText(),new JsonSerializerOptions {PropertyNamingPolicy=JsonNamingPolicy.CamelCase})!;
            typed.Validate(); Assert.Equal(.000001,typed.ReconnectDelaySeconds);
            Input("directReadChunkKiB").Text="3"; Assert.Throws<ArgumentException>(()=>form.Read());
            Input("directReadChunkKiB").Text="64"; Assert.Equal(64,form.Read().GetProperty("directReadChunkKiB").GetInt32());
            Input("maxRetries").Text="1.5"; Assert.Throws<ArgumentException>(()=>form.Read());
            Input("maxRetries").Text="3";
            Input("reconnectDelaySeconds").Text="0"; Assert.Throws<ArgumentException>(()=>form.Read());
            Input("reconnectDelaySeconds").Text="5";
            var root=new DockPanel();
            var notice=new TextBlock {Text="Recovery editor preview · no equipment",Margin=new Thickness(16),FontSize=18};
            DockPanel.SetDock(notice,Dock.Top);root.Children.Add(notice);root.Children.Add(tabs);
            var window=new Window {Title="Regain shared recovery editor",Content=root,Width=720,Height=740};
            SetupTheme.Apply(window);
            try {window.Show();await Capture(window,"camera-recovery-native.png");}
            finally {window.Close();}
        });
    }
}
