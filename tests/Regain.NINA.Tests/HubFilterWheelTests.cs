using System.Text.Json;
using Moq;
using NINA.Core.Model.Equipment;
using NINA.Core.Utility;
using NINA.Equipment.Interfaces;
using NINA.Profile.Interfaces;
using Regain.Hub;
using Regain.TestFixtures;
using Xunit;

namespace Regain.NINA.Tests;

public sealed partial class HubNativeTests
{
    private static (IProfileService Profiles, ObserveAllCollection<FilterInfo> Filters) WheelProfile()
    {
        var profiles = new Mock<IProfileService>();
        var filters = new ObserveAllCollection<FilterInfo> {new FilterInfo("Existing L",12,0)};
        profiles.SetupGet(profile => profile.ActiveProfile.FilterWheelSettings.FilterWheelFilters).Returns(filters);
        return (profiles.Object,filters);
    }
    [Fact]
    public async Task NativeWheelPreservesProfileMetadataAndSharesNonblockingMotion()
    {
        // NINA collections capture the application dispatcher. Use a thread
        // without xUnit's synchronization context, as for the direct wheel.
        await Task.Run(async () => {
            using var source = new HubFilterWheelServer();
            await using var host = await Host.Open(config => source.AddTo(config,4,17));
            var (profiles,filters) = WheelProfile(); var original = filters[0];
            using var first = new HubFilterWheelDevice(profiles,host.Selection(3,"filterwheel"),host.Executable,host.Workers);
            using var sibling = new HubFilterWheelDevice(profiles,host.Selection(4,"filterwheel"),host.Executable,host.Workers);
            Assert.False(first.Connected); Assert.Equal(0,source.Moves);
            await first.Connect(CancellationToken.None); await sibling.Connect(CancellationToken.None);
            Assert.Equal(new[] {"L","Hα",""},first.Names); Assert.Equal(new[] {-12,0,17},sibling.FocusOffsets);
            Assert.Same(original,filters[0]); Assert.Equal("Existing L",filters[0].Name); Assert.Equal(3,filters.Count);
            Assert.Equal("Hα",filters[1].Name); Assert.Equal("",filters[2].Name);
            var copy = first.Names; copy[0] = "local change"; Assert.Equal("L",sibling.Names[0]);
            var offsets = first.FocusOffsets; offsets[0] = 999; Assert.Equal(-12,sibling.FocusOffsets[0]);
            Assert.Throws<ArgumentOutOfRangeException>(() => first.Position = -1);
            Assert.Equal("invalidValue",Assert.Throws<HubException>(() => first.Position = 3).Remote!.Code);
            Assert.Equal(0,source.Moves);
            first.Position = 2; Assert.Equal(1,source.Moves); Assert.Equal(-1,sibling.Position);
            Assert.Equal("busy",Assert.Throws<HubException>(() => sibling.Position = 1).Remote!.Code);
            source.Values["position"] = 1; Assert.Equal(1,first.Position); // Never invent the requested target.
            source.Values["position"] = 2; Assert.Equal(2,sibling.Position);
            first.Disconnect(); Assert.True(sibling.Connected); Assert.Equal(2,sibling.Position);
            Assert.Equal(0,source.Halts); Assert.Empty(first.SupportedActions);
            Assert.Throws<NotSupportedException>(() => sibling.Action("Regain.Calibrate",""));
            sibling.Disconnect();
            await Eventually(async () => (await host.Command(new {op="sourceStatus",source=source.SourceId})).GetProperty("leaseCount").GetInt32() == 0);
        });
    }
    [Fact]
    public async Task NativeWheelFailedAdmissionAndMalformedReadingsPreserveProfileAndNeverMove()
    {
        await Task.Run(async () => {
            using var source = new HubFilterWheelServer();
            await using var host = await Host.Open(config => source.AddTo(config,4));
            var (profiles,filters) = WheelProfile(); var original = filters[0];
            using var wheel = new HubFilterWheelDevice(profiles,host.Selection(3,"filterwheel"),host.Executable,host.Workers);
            source.Values["focusoffsets"] = new[] {0};
            Assert.Equal("unavailable",(await Assert.ThrowsAsync<HubException>(() => wheel.Connect(CancellationToken.None))).Remote!.Code);
            Assert.False(wheel.Connected); Assert.Single(filters); Assert.Same(original,filters[0]);
            await Eventually(async () => (await host.Command(new {op="sourceStatus",source=source.SourceId})).GetProperty("leaseCount").GetInt32() == 0);
            source.Values["focusoffsets"] = new[] {-12,0,17};
            await wheel.Connect(CancellationToken.None);
            source.Values["position"] = "0";
            Assert.Equal("unavailable",Assert.Throws<HubException>(() => _ = wheel.Position).Remote!.Code);
            Assert.Equal("unavailable",Assert.Throws<HubException>(() => wheel.Position = 2).Remote!.Code);
            Assert.Equal(0,source.Moves); Assert.True(wheel.Connected); Assert.NotEmpty(wheel.LastError);
            source.Values["position"] = 0; Assert.Equal(0,wheel.Position);
        });
    }
    [Fact]
    public async Task NativeWheelLostPositionReplyRetainsSharedUncertaintyWithoutReplay()
    {
        await Task.Run(async () => {
            using var source = new HubFilterWheelServer {LoseMoveReply=true};
            await using var host = await Host.Open(config => source.AddTo(config,4,17));
            var (profiles,_) = WheelProfile();
            using var first = new HubFilterWheelDevice(profiles,host.Selection(3,"filterwheel"),host.Executable,host.Workers);
            using var sibling = new HubFilterWheelDevice(profiles,host.Selection(4,"filterwheel"),host.Executable,host.Workers);
            await first.Connect(CancellationToken.None); await sibling.Connect(CancellationToken.None);
            var lost = Assert.Throws<HubException>(() => first.Position = 2);
            await AssertAccessoryFailure(host,source,lost,"uncertain","wheel lost Position reply");
            var fenced = Assert.Throws<HubException>(() => sibling.Position = 1);
            await AssertAccessoryFailure(host,source,fenced,"uncertain","wheel sibling Position fence");
            Assert.False(first.Connected); Assert.False(sibling.Connected);
            Assert.Equal(1,source.Moves); Assert.Equal(0,source.Halts);
            Assert.True((await host.Command(new {op="sourceStatus",source=source.SourceId})).GetProperty("writeUncertain").GetBoolean());
        });
    }
    [Fact]
    public async Task NativeWheelProfileChangeDuringConnectCannotPublishIntoEitherProfile()
    {
        await Task.Run(async () => {
            using var source = new HubFilterWheelServer();
            await using var host = await Host.Open(config => source.AddTo(config,4));
            var original = new ObserveAllCollection<FilterInfo> {new FilterInfo("Original",12,0)};
            var replacement = new ObserveAllCollection<FilterInfo> {new FilterInfo("Replacement",34,0)};
            var profiles = new Mock<IProfileService>(); int reads = 0;
            profiles.SetupGet(profile => profile.ActiveProfile.FilterWheelSettings.FilterWheelFilters)
                .Returns(() => Interlocked.Increment(ref reads) == 1 ? original : replacement);
            using var wheel = new HubFilterWheelDevice(profiles.Object,host.Selection(3,"filterwheel"),host.Executable,host.Workers);
            await Assert.ThrowsAsync<InvalidOperationException>(() => wheel.Connect(CancellationToken.None));
            Assert.False(wheel.Connected); Assert.Equal(0,source.Moves);
            Assert.Single(original); Assert.Single(replacement);
            Assert.Equal("Original",original[0].Name); Assert.Equal("Replacement",replacement[0].Name);
            await Eventually(async () => (await host.Command(new {op="sourceStatus",source=source.SourceId})).GetProperty("leaseCount").GetInt32() == 0);
        });
    }
    [Fact]
    public async Task NativeWheelChoicesAndWireValidationNeedNoEquipment()
    {
        await Task.Run(() => {
        var directory = Path.Combine(Path.GetTempPath(),"Regain wheel choices "+Guid.NewGuid().ToString("N")); Directory.CreateDirectory(directory);
        try {
            var (profiles,_) = WheelProfile(); var store = new HubSelectionStore(Path.Combine(directory,"bindings.json"));
            var binding = Binding(directory,"filterwheel"); store.Save(binding,Guid.Empty);
            var choices = HubEquipment.Choices<IFilterWheel>("filterwheel",selection => new HubFilterWheelDevice(profiles,selection),store);
            Assert.Equal(binding.Id,choices[0].Id); Assert.EndsWith("Configure.filterwheel",choices[1].Id);
            foreach (var choice in choices) ((IDisposable)choice).Dispose();
            using var descriptor = JsonDocument.Parse(File.ReadAllText(Path.Combine(AppContext.BaseDirectory,"hub-config.json")));
            Assert.Equal(HubFilterWheelProtocol.MaximumSlots-1,descriptor.RootElement.GetProperty("outputDiagnostics").GetProperty("filterwheelProperties")[2].GetProperty("maximum").GetInt32());
            foreach (var (property,json) in new[] {
                (HubFilterWheelProperty.Names,"[]"),(HubFilterWheelProperty.Names,"[1]"),(HubFilterWheelProperty.Names,"[[\"L\"]]"),
                (HubFilterWheelProperty.Names,"[\"\\uD800\"]"),
                (HubFilterWheelProperty.Names,JsonSerializer.Serialize(Enumerable.Repeat("L",1025))),
                (HubFilterWheelProperty.Names,JsonSerializer.Serialize(new[] {new string('α',524289)})),
                (HubFilterWheelProperty.FocusOffsets,"[]"),(HubFilterWheelProperty.FocusOffsets,"[1,2]"),
                (HubFilterWheelProperty.FocusOffsets,"[0,1.5]"),(HubFilterWheelProperty.FocusOffsets,"[0,2147483648]"),
                (HubFilterWheelProperty.FocusOffsets,"[0,-2147483649]"),(HubFilterWheelProperty.FocusOffsets,"[0,\"1\"]"),
                (HubFilterWheelProperty.Position,"-2"),(HubFilterWheelProperty.Position,"1024"),(HubFilterWheelProperty.Position,"0.5") }) {
                using var value = JsonDocument.Parse(json);
                Assert.Throws<HubException>(() => HubFilterWheelProtocol.Validate(property,value.RootElement));
            }
            using var names = JsonDocument.Parse("[\"L\",\"Hα\",\"\"]");
            using var offsets = JsonDocument.Parse("[-2147483648,0,2147483647]");
            var metadata = HubFilterWheelProtocol.Metadata(names.RootElement,offsets.RootElement);
            Assert.Equal(new[] {"L","Hα",""},metadata.Names); Assert.Equal(new[] {int.MinValue,0,int.MaxValue},metadata.FocusOffsets);
            Assert.Throws<ArgumentOutOfRangeException>(() => HubFilterWheelProtocol.Move(1024));
        } finally {Directory.Delete(directory,true);}
        });
    }
}
