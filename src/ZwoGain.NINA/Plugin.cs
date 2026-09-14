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
using ZwoGain.Core;

[assembly: ComVisible(false)]
[assembly: Guid("6953efde-7f7e-48df-94d5-671986293974")]
[assembly: AssemblyMetadata("MinimumApplicationVersion", "3.2.0.9001")]
[assembly: AssemblyMetadata("License", "Apache-2.0")]
[assembly: AssemblyMetadata("LicenseURL", "https://github.com/theatrus/zwogain/blob/main/LICENSE")]
[assembly: AssemblyMetadata("Repository", "https://github.com/theatrus/zwogain")]
[assembly: AssemblyMetadata("Homepage", "https://github.com/theatrus/zwogain")]
[assembly: AssemblyMetadata("ChangelogURL", "https://github.com/theatrus/zwogain/releases")]
[assembly: AssemblyMetadata("Tags", "camera,ZWO,recovery")]
[assembly: AssemblyMetadata("FeaturedImageURL", "pack://application:,,,/ZwoGain.NINA;component/Assets/zwogain.png")]
[assembly: AssemblyMetadata("ShortDescription", "ZWO camera driver with automatic retries")]
[assembly: AssemblyMetadata("LongDescription", "Retries failed captures and restores camera settings after reconnecting.")]

namespace ZwoGain.NINA;

[Export(typeof(IPluginManifest))]
public sealed class ZwoGainPlugin : PluginBase
{
}

[Export(typeof(IEquipmentProvider))]
public sealed class CameraProvider : IEquipmentProvider<ICamera>
{
    private readonly IExposureDataFactory images;
    private readonly IProfileService profiles;
    public string Name => "ZWOgain";
    [ImportingConstructor]
    public CameraProvider(IExposureDataFactory images, IProfileService profiles)
    {
        this.images = images;
        this.profiles = profiles;
    }
    internal static readonly string DirectoryPath = Path.GetDirectoryName(typeof(CameraProvider).Assembly.Location)!;
    internal static HostClient NewHost() => new(Path.Combine(DirectoryPath, "zwogain-host.exe"), Path.Combine(DirectoryPath, "ASICamera2.dll"), log: message => Logger.Debug("ZWOgain SDK: " + message));
    internal static HostClient NewDirectHost() => new(Path.Combine(DirectoryPath, "zwogain-direct.exe"), "unused", direct: true, log: message => Logger.Debug("ZWOgain direct: " + message));
    internal static async Task<List<CameraDescriptor>> DiscoverAsync(CancellationToken token, bool direct = false)
    {
        using var host = direct ? NewDirectHost() : NewHost();
        var reply = await host.CallAsync("list", null, TimeSpan.FromSeconds(15), token).ConfigureAwait(false);
        return reply.Result.EnumerateArray().Select(CameraDescriptor.Parse).ToList();
    }
    // Always available so setup also works before USB attachment.
    public IList<ICamera> GetEquipment() => [new ResilientCamera(images, Settings.Cameras, profiles: profiles)];
}
