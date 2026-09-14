using System.ComponentModel.Composition;
using System.Reflection;
using System.Runtime.InteropServices;
using NINA.Plugin;
using NINA.Plugin.Interfaces;
using NINA.Equipment.Interfaces;
using NINA.Equipment.Interfaces.ViewModel;
using NINA.Image.Interfaces;
using NINA.Core.Utility;
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
[assembly: AssemblyMetadata("ShortDescription", "ZWO camera driver with automatic capture recovery")]
[assembly: AssemblyMetadata("LongDescription", "Runs the ASI SDK in a supervised Rust process. Recovers failed exposures and downloads, restores controls and cooling, and retries the original capture transparently.")]

namespace ZwoGain.NINA;

[Export(typeof(IPluginManifest))]
public sealed class ZwoGainPlugin : PluginBase
{
}

[Export(typeof(IEquipmentProvider))]
public sealed class CameraProvider : IEquipmentProvider<ICamera>
{
    private readonly IExposureDataFactory images;
    public string Name => "ZwoGain";
    [ImportingConstructor]
    public CameraProvider(IExposureDataFactory images)
    {
        this.images = images;
    }
    internal static readonly string DirectoryPath = Path.GetDirectoryName(typeof(CameraProvider).Assembly.Location)!;
    internal static HostClient NewHost() => new(Path.Combine(DirectoryPath, "zwogain-host.exe"), Path.Combine(DirectoryPath, "ASICamera2.dll"), log: message => Logger.Debug("ZwoGain SDK: " + message));
    internal static async Task<List<CameraDescriptor>> DiscoverAsync(CancellationToken token)
    {
        using var host = NewHost();
        var reply = await host.CallAsync("list", null, TimeSpan.FromSeconds(15), token).ConfigureAwait(false);
        return reply.Result.EnumerateArray().Select(CameraDescriptor.Parse).ToList();
    }
    // Always available so setup also works before USB attachment.
    public IList<ICamera> GetEquipment() => [new ResilientCamera(images, Settings.Cameras)];
}
