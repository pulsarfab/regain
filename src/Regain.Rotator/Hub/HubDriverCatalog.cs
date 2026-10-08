using System.IO;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Text;
using Microsoft.Win32;

namespace Regain.Hub.ASCOM;

/// Read-only seam shared by real architecture-specific registry discovery and
/// private fixtures. Nothing here creates a COM driver or reads equipment.
public interface IHubDriverRegistry
{
    IEnumerable<string> Drivers(string deviceType);
    string? Description(string deviceType, string progId);
    Guid? ClassId(string progId);
}
public sealed class HubDriverCatalog
{
    public sealed class Entry
    {
        public string Name { get; }
        public string ProgId { get; }
        public Guid? ClassId { get; }
        internal Entry(string name, string progId, Guid? classId) { Name = name; ProgId = progId; ClassId = classId; }
    }
    public IReadOnlyList<Entry> Entries { get; }
    public bool Incomplete { get; }
    private HubDriverCatalog(List<Entry> entries, bool incomplete) { Entries = entries; Incomplete = incomplete; }
    public static HubDriverCatalog Read(IHubDriverRegistry registry, string deviceType)
    {
        if (!HubSelection.Types.Contains(deviceType)) throw new ArgumentException("Unsupported driver class");
        var entries = new List<Entry>(); var identities = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        var incomplete = false; var scanned = 0;
        try {
            foreach (var progId in registry.Drivers(deviceType)) {
                if (++scanned > 4096) { incomplete = true; break; }
                if (progId.Length is 0 or > 200 || !progId.All(c => c is >= 'A' and <= 'Z' or >= 'a' and <= 'z' or >= '0' and <= '9' or '.' or '_' or '-')) {
                    incomplete = true; continue;
                }
                if (!identities.Add(progId)) continue;
                if (entries.Count == 256) { incomplete = true; break; }
                string? name = null; Guid? clsid = null;
                try { name = registry.Description(deviceType, progId); clsid = registry.ClassId(progId); }
                catch { incomplete = true; }
                if (string.IsNullOrWhiteSpace(name) || name!.Any(char.IsControl)) name = progId;
                if (name!.Length > 200) name = name.Substring(0, char.IsHighSurrogate(name[199]) ? 199 : 200);
                if (clsid == Guid.Empty) { clsid = null; incomplete = true; }
                entries.Add(new Entry(name, progId, clsid));
            }
        } catch { incomplete = true; }
        entries.Sort((a, b) => { var order = StringComparer.Ordinal.Compare(a.Name, b.Name); return order != 0 ? order : StringComparer.OrdinalIgnoreCase.Compare(a.ProgId, b.ProgId); });
        return new HubDriverCatalog(entries, incomplete);
    }
}

public sealed class HubSystemDriverRegistry : IHubDriverRegistry, IDisposable
{
    private readonly RegistryKey machine, user, classes;
    private readonly bool elevated;
    public HubSystemDriverRegistry()
    {
        using var identity = WindowsIdentity.GetCurrent();
        elevated = new WindowsPrincipal(identity).IsInRole(WindowsBuiltInRole.Administrator);
        machine = RegistryKey.OpenBaseKey(RegistryHive.LocalMachine, RegistryView.Default);
        user = RegistryKey.OpenBaseKey(RegistryHive.CurrentUser, RegistryView.Default);
        classes = RegistryKey.OpenBaseKey(RegistryHive.ClassesRoot, RegistryView.Default);
    }
    // Private fixture roots preserve the actual registry API and elevation
    // precedence without touching production ASCOM registrations.
    internal HubSystemDriverRegistry(RegistryKey machine, RegistryKey user, RegistryKey classes, bool elevated)
    { this.machine = machine; this.user = user; this.classes = classes; this.elevated = elevated; }
    private static string Category(string type) => type switch {
        "camera" => "Camera", "switch" => "Switch", "safetymonitor" => "SafetyMonitor", "observingconditions" => "ObservingConditions",
        "focuser" => "Focuser", "rotator" => "Rotator", "filterwheel" => "FilterWheel", "covercalibrator" => "CoverCalibrator", _ => throw new ArgumentException()
    };
    public IEnumerable<string> Drivers(string deviceType)
    {
        var path = @"Software\ASCOM\" + Category(deviceType) + " Drivers";
        foreach (var root in elevated ? new[] { machine } : new[] { user, machine }) {
            using var key = root.OpenSubKey(path, false);
            if (key is null) continue;
            // Do not allocate GetSubKeyNames' unbounded array. An extra item
            // communicates scan exhaustion to the shared bounded collector.
            for (uint index = 0; index <= 4096; index++) {
                var name = new StringBuilder(256); uint length = 256;
                var error = RegEnumKeyEx(key.Handle, index, name, ref length, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
                if (error == 259) break;
                if (error != 0) throw new IOException("Registry enumeration unavailable");
                yield return name.ToString();
            }
        }
    }
    public string? Description(string deviceType, string progId)
    {
        var path = @"Software\ASCOM\" + Category(deviceType) + @" Drivers\" + progId;
        foreach (var root in elevated ? new[] { machine } : new[] { user, machine }) {
            using var key = root.OpenSubKey(path, false);
            if (key is not null) return key.GetValue(null, null, RegistryValueOptions.DoNotExpandEnvironmentNames) as string;
        }
        return null;
    }
    public Guid? ClassId(string progId)
    {
        using var key = (elevated ? machine : classes).OpenSubKey((elevated ? @"Software\Classes\" : "") + progId + @"\CLSID", false);
        return key?.GetValue(null, null, RegistryValueOptions.DoNotExpandEnvironmentNames) is string value && Guid.TryParse(value, out var id) && id != Guid.Empty ? id : null;
    }
    public void Dispose() { machine.Dispose(); user.Dispose(); classes.Dispose(); }
    [DllImport("advapi32.dll", CharSet = CharSet.Unicode)]
    private static extern int RegEnumKeyEx(Microsoft.Win32.SafeHandles.SafeRegistryHandle key, uint index, StringBuilder name, ref uint nameLength,
        IntPtr reserved, IntPtr className, IntPtr classLength, IntPtr lastWriteTime);
}
