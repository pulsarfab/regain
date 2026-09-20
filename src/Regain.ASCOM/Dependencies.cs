using System.Reflection;

namespace Regain.Ascom;

internal static class Dependencies
{
    // A COM DLL cannot supply binding redirects in the client application's config.
    // Resolve compatible updates from our own directory. COM binding may omit
    // RequestingAssembly, so those requests are limited to the bundled packages.
    private static readonly HashSet<string> Packages = new(StringComparer.Ordinal) {
        "ASCOM.DeviceInterfaces", "ASCOM.Exceptions",
        "Regain.Rotator",
        "Microsoft.Bcl.AsyncInterfaces", "System.Buffers", "System.IO.Pipelines", "System.Memory",
        "System.Numerics.Vectors", "System.Runtime.CompilerServices.Unsafe", "System.Text.Encodings.Web",
        "System.Text.Json", "System.Threading.Tasks.Extensions"
    };
    internal static void Install() => AppDomain.CurrentDomain.AssemblyResolve += Resolve;

    private static Assembly? Resolve(object sender, ResolveEventArgs args)
    {
        string directory = Path.GetDirectoryName(typeof(Dependencies).Assembly.Location)!;
        if (args.RequestingAssembly is not null &&
            !string.Equals(Path.GetDirectoryName(args.RequestingAssembly.Location), directory, StringComparison.OrdinalIgnoreCase)) return null;
        var requested = new AssemblyName(args.Name);
        if (!Packages.Contains(requested.Name)) return null;
        string file = Path.Combine(directory, requested.Name + ".dll");
        if (!File.Exists(file)) return null;
        var available = AssemblyName.GetAssemblyName(file);
        if (available.Name != requested.Name || available.CultureName != requested.CultureName || available.Version.Major != requested.Version.Major || available.Version < requested.Version ||
            !(available.GetPublicKeyToken() ?? []).SequenceEqual(requested.GetPublicKeyToken() ?? [])) return null;
        return Assembly.LoadFrom(file);
    }
}
