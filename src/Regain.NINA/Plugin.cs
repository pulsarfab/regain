using System.ComponentModel.Composition;
using System.Reflection;
using System.Runtime.InteropServices;
using NINA.Plugin;
using NINA.Plugin.Interfaces;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.ViewModel;
using NINA.Image.Interfaces;
using NINA.Core.Utility;
using NINA.Profile.Interfaces;
using Regain.Core;

[assembly: ComVisible(false)]
[assembly: Guid("6953efde-7f7e-48df-94d5-671986293974")]
[assembly: AssemblyMetadata("MinimumApplicationVersion", "3.2.0.9001")]
[assembly: AssemblyMetadata("License", "Apache-2.0")]
[assembly: AssemblyMetadata("LicenseURL", "https://github.com/pulsarfab/regain/blob/main/LICENSE")]
[assembly: AssemblyMetadata("Repository", "https://github.com/pulsarfab/regain")]
[assembly: AssemblyMetadata("Homepage", "https://github.com/pulsarfab/regain")]
[assembly: AssemblyMetadata("ChangelogURL", "https://github.com/pulsarfab/regain/releases")]
[assembly: AssemblyMetadata("Tags", "camera,rotator,filterwheel,focuser,ZWO,Pegasus,FocusCube3,recovery")]
[assembly: AssemblyMetadata("FeaturedImageURL", "pack://application:,,,/Regain.NINA;component/Assets/regain.png")]
[assembly: AssemblyMetadata("ShortDescription", "Regain control of your equipment.")]
[assembly: AssemblyMetadata("LongDescription", "PulsarFab regain brings resilient camera capture and native equipment control to NINA. Connect ZWO cameras, CAA rotators, EFW filter wheels, EAF and Pegasus FocusCube3 focusers with Rust drivers and matching setup dialogs.")]

namespace Regain.NINA;

[Export(typeof(IPluginManifest))]
public sealed class RegainPlugin : PluginBase
{
}

[Export(typeof(IEquipmentProvider))]
public sealed class CameraProvider : IEquipmentProvider<ICamera>
{
    private readonly IExposureDataFactory images;
    private readonly IProfileService profiles;
    public string Name => "PulsarFab regain";
    [ImportingConstructor]
    public CameraProvider(IExposureDataFactory images, IProfileService profiles)
    {
        this.images = images;
        this.profiles = profiles;
    }
    internal static readonly string DirectoryPath = Path.GetDirectoryName(typeof(CameraProvider).Assembly.Location)!;
    internal static HostClient NewHost() => new(Path.Combine(DirectoryPath, "regain-alpaca.exe"), Path.Combine(DirectoryPath, "ASICamera2.dll"), supervised: true, log: message => CameraLog.Worker("SDK", message));
    internal static HostClient NewDirectHost() => new(Path.Combine(DirectoryPath, "regain-alpaca.exe"), Path.Combine(DirectoryPath, "ASICamera2.dll"), direct: true, supervised: true, log: message => CameraLog.Worker("direct", message));
    internal static async Task<List<CameraDescriptor>> DiscoverAsync(CancellationToken token, bool direct = false)
    {
        using var host = direct ? NewDirectHost() : NewHost();
        var reply = await host.CallAsync("list", null, TimeSpan.FromSeconds(15), token).ConfigureAwait(false);
        return reply.Result.EnumerateArray().Select(CameraDescriptor.Parse).ToList();
    }
    // Always available so setup also works before USB attachment.
    public IList<ICamera> GetEquipment() => [new ResilientCamera(images, Settings.Cameras, profiles: profiles)];
}
