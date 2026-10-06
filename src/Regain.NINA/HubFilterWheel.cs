using System.ComponentModel.Composition;
using System.Text.Json;
using NINA.Core.Model.Equipment;
using NINA.Core.Utility;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.ViewModel;
using NINA.Profile.Interfaces;
using Regain.Hub;

namespace Regain.NINA;

[Export(typeof(IEquipmentProvider))]
public sealed class HubFilterWheelProvider : IEquipmentProvider<IFilterWheel>
{
    private readonly IProfileService profiles;
    [ImportingConstructor] public HubFilterWheelProvider(IProfileService profiles) => this.profiles = profiles;
    public string Name => "PulsarFab regain";
    public IList<IFilterWheel> GetEquipment() => HubEquipment.Choices<IFilterWheel>("filterwheel", binding => new HubFilterWheelDevice(profiles,binding));
}

public sealed class HubFilterWheelDevice : HubTypedDevice, IFilterWheel
{
    private readonly IProfileService profiles;
    public HubFilterWheelDevice(IProfileService profiles,HubSelection? selection,string? executable = null,string? workers = null)
        : base("filterwheel",selection,executable,workers) => this.profiles = profiles;
    private Task<JsonElement> Read(Guid epoch,Guid output,HubFilterWheelProperty property,CancellationToken token)
        => ReadTyped(epoch,output,HubFilterWheelProtocol.Read(property),value => HubFilterWheelProtocol.Validate(property,value),token);
    private JsonElement Read(HubFilterWheelProperty property)
    {
        var context = RequireContext();
        return Read(context.Epoch,context.Binding.OutputId,property,CancellationToken.None).GetAwaiter().GetResult();
    }
    public string[] Names => Read(HubFilterWheelProperty.Names).EnumerateArray().Select(item => item.GetString()!).ToArray();
    public int[] FocusOffsets => Read(HubFilterWheelProperty.FocusOffsets).EnumerateArray().Select(item => item.GetInt32()).ToArray();
    public AsyncObservableCollection<FilterInfo> Filters => profiles.ActiveProfile.FilterWheelSettings.FilterWheelFilters;
    protected override async Task<Action> Prepare(Guid epoch,Guid output,CancellationToken token)
    {
        var filters = Filters;
        var names = await Read(epoch,output,HubFilterWheelProperty.Names,token).ConfigureAwait(false);
        var offsets = await Read(epoch,output,HubFilterWheelProperty.FocusOffsets,token).ConfigureAwait(false);
        var metadata = HubFilterWheelProtocol.Metadata(names,offsets);
        return () => {
            if (!ReferenceEquals(filters,Filters)) throw new InvalidOperationException("NINA profile changed while connecting the wheel; connect explicitly using the current profile");
            // Match the direct wheel: retain NINA's existing exposure/autofocus
            // settings and initialize only missing slots from source metadata.
            for (int index = filters.Count; index < metadata.Names.Length; index++)
                filters.Add(new FilterInfo(metadata.Names[index],metadata.FocusOffsets[index],checked((short)index)));
            while (filters.Count > metadata.Names.Length) filters.RemoveAt(filters.Count - 1);
        };
    }
    public short Position {
        get => checked((short)Read(HubFilterWheelProperty.Position).GetInt32());
        set {
            var property = HubFilterWheelProtocol.Move(value); var context = RequireContext();
            WriteTyped(context.Epoch,context.Binding.OutputId,property,CancellationToken.None).GetAwaiter().GetResult();
            // IFilterWheel exposes a nonblocking setter. NINA observes Position
            // (-1 while moving); an acknowledgment never fabricates completion.
            RaiseAllPropertiesChanged();
        }
    }
}
