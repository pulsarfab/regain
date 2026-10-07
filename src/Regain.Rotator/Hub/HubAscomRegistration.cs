using System.IO;
using System.Security.Principal;
using System.Runtime.InteropServices;
using Microsoft.Win32;

namespace Regain.Hub.ASCOM;

/// Registry publication contains no equipment operations. The caller holds the
/// machine registration mutex and supplies both views (or private test roots).
public static class HubAscomRegistration
{
    public const string Inventory = @"Software\PulsarFab\Regain\HubExports";
    public static string CurrentOwner { get { using var identity = WindowsIdentity.GetCurrent(); return identity.User!.Value; } }
    private static readonly Version version = typeof(HubAscomRegistration).Assembly.GetName().Version!;

    public static Guid RegisterSaved(IReadOnlyList<RegistryKey> roots, string directory, string selections,
        Guid revision, Guid instance, Guid output, string owner)
    {
        if (owner != CurrentOwner) throw new InvalidOperationException("Register using the original user's Windows account");
        directory = FullPath(directory); selections = FullPath(selections);
        var server = Path.Combine(directory, "hub-ascom", "x64", "Regain.Hub.ASCOM.exe");
        var host = Path.Combine(directory, "regain-alpaca.exe");
        if (!File.Exists(server) || !File.Exists(host)) throw new InvalidOperationException("Installed hub executables are missing");
        return new HubSelectionStore(selections).WithRevision(revision, saved => {
            var binding = saved.Bindings.SingleOrDefault(b => b.InstanceId == instance && b.OutputId == output)?.Copy()
                ?? throw new InvalidOperationException("The saved output no longer exists; reload selections");
            var record = new Record(binding, directory, selections, owner, version);
            if (record.CommandLength > 32700) throw new InvalidOperationException("Registered command exceeds the Windows command-line limit");
            if (roots.Any(root => root.Name == "HKEY_LOCAL_MACHINE")) {
                foreach (var view in new[] { RegistryView.Registry32, RegistryView.Registry64 }) {
                    using var user = RegistryKey.OpenBaseKey(RegistryHive.CurrentUser, view);
                    foreach (var path in record.TopKeys().Where(path => path != record.InventoryKey)) {
                        using var overlay = user.OpenSubKey(path);
                        if (overlay is not null) throw new InvalidOperationException("A per-user registration shadows this output; reconcile it before registration");
                    }
                }
            }
            Mutate(roots, record, false);
            return record.Id;
        });
    }

    /// Removal uses the inventory, not the mutable selection file. An installer
    /// can pass owner=null to remove all users' entries for this exact install.
    public static void Remove(IReadOnlyList<RegistryKey> roots, string directory, Guid id, string? owner)
    {
        directory = FullPath(directory);
        if (owner is not null && owner != CurrentOwner) throw new InvalidOperationException("Remove using the original user's Windows account");
        var record = FindRemoval(roots, directory, id, owner);
        if (record is not null) Mutate(roots, record, true);
    }

    /// Installer cleanup preflights the complete install before any deletion,
    /// and caught mutation failures attempt restoration of every owned output.
    public static void RemoveAll(IReadOnlyList<RegistryKey> roots, string directory)
    {
        directory = FullPath(directory);
        var ids = roots.SelectMany(root => RegisteredIds(root, directory)).Distinct().ToArray();
        if (ids.Length > 256) throw new InvalidOperationException("Installation inventory exceeds the batch removal limit");
        var records = ids.Select(id => FindRemoval(roots, directory, id, null)).Where(record => record is not null).Cast<Record>().ToArray();
        if (records.Length != 0) MutateMany(roots, records, true);
    }

    private static Record? FindRemoval(IReadOnlyList<RegistryKey> roots, string directory, Guid id, string? owner)
    {
        Record? record = null;
        foreach (var root in roots) {
            using var key = root.OpenSubKey(Inventory + "\\" + id.ToString("B"));
            if (key is null) continue;
            var candidate = Record.Read(key);
            if (candidate.Id != id || !SamePath(candidate.Directory, directory) || (owner is not null && candidate.Owner != owner)
                || candidate.Version > version) throw new InvalidOperationException("Registration belongs to another user, install or newer version");
            if (record is not null && !record.SameIdentity(candidate)) throw new InvalidOperationException("Registry views disagree; inspect registration before removal");
            record = candidate;
        }
        return record;
    }

    public static Guid[] RegisteredIds(RegistryKey root, string? directory = null)
        => RegisteredOutputs(root, directory).Select(output => output.ClassId).ToArray();

    public static HubRegisteredOutput[] RegisteredOutputs(RegistryKey root, string? directory = null)
    {
        if (directory is not null) directory = FullPath(directory);
        using var inventory = root.OpenSubKey(Inventory);
        if (inventory is null) return [];
        var result = new List<HubRegisteredOutput>();
        var names = inventory.GetSubKeyNames();
        if (names.Length > 4096) throw new InvalidOperationException("Hub registration inventory is too large");
        foreach (var name in names) {
            using var key = inventory.OpenSubKey(name)!;
            // A scoped installer must leave another installation's future
            // schema alone, rather than attempting to interpret/remove it.
            var installedDirectory = key.GetValue("InstallDirectory") as string;
            if (directory is not null && installedDirectory is not null && !SamePath(FullPath(installedDirectory), directory)) continue;
            var id = Guid.ParseExact(name, "B");
            var record = Record.Read(key);
            if (record.Id != id) throw new InvalidOperationException("Inventory key and output identity disagree");
            if (directory is null || SamePath(record.Directory, directory))
                result.Add(new HubRegisteredOutput(id, record.Selection, record.Directory, record.Selections,
                    record.Owner, record.Version, key.GetValue("Phase") as string ?? ""));
        }
        return result.ToArray();
    }

    private static void Mutate(IReadOnlyList<RegistryKey> roots, Record record, bool remove)
        => MutateMany(roots, [record], remove);

    private static void MutateMany(IReadOnlyList<RegistryKey> roots, IReadOnlyList<Record> records, bool remove)
    {
        if (roots.Count != 2) throw new ArgumentException("Both registry views are required");
        var backups = new List<(RegistryKey Root, string Path, Tree? Before)>();
        var budget = new SnapshotBudget(Math.Min(512 * records.Count, 4096), Math.Min(1024L * 1024 * records.Count, 8L * 1024 * 1024));
        // Preflight every view before writing anything, including a collision
        // hidden in the second view. Existing registrations must prove ownership.
        foreach (var root in roots) {
            foreach (var record in records) {
                var top = record.TopKeys();
                using var inventory = root.OpenSubKey(record.InventoryKey);
                if (inventory is null) {
                    foreach (var path in top) { using var present = root.OpenSubKey(path); if (present is not null) throw new InvalidOperationException("An unowned registration already exists"); }
                } else {
                    var installed = Record.Read(inventory);
                    if (!record.SameIdentity(installed) || installed.Version > version)
                        throw new InvalidOperationException("Registration belongs to another user, install or newer version");
                    // Missing values allow explicit recovery of an owned pending
                    // registration; conflicting values never authorize overwrite.
                    foreach (var check in installed.CriticalValues()) {
                        using var key = root.OpenSubKey(check.Path);
                        var value = key?.GetValue(check.Name);
                        if (value is not null && !Equals(value, check.Value)) throw new InvalidOperationException("Registered command or identity changed; inspect before editing");
                    }
                }
                foreach (var path in top) { using var key = root.OpenSubKey(path); backups.Add((root, path, key is null ? null : Tree.Read(key, budget, 0))); }
            }
        }
        try {
            foreach (var root in roots) {
                foreach (var record in records) {
                    var top = record.TopKeys();
                    if (remove) {
                        foreach (var path in top.Where(path => path != record.InventoryKey)) root.DeleteSubKeyTree(path, false);
                        root.DeleteSubKeyTree(record.InventoryKey, false);
                    } else {
                        // Persist ownership first so an interrupted registry edit
                        // is identifiable and explicitly repairable/removable.
                        Write(root, record.InventoryKey, record.InventoryValues("pending"));
                        foreach (var entry in record.Entries()) Write(root, entry.Key, entry.Value);
                        Write(root, record.InventoryKey, record.InventoryValues("ready"));
                    }
                }
            }
        } catch (Exception error) {
            var failures = new List<Exception> { error };
            foreach (var backup in backups.AsEnumerable().Reverse()) {
                try {
                    backup.Root.DeleteSubKeyTree(backup.Path, false);
                    if (backup.Before is not null) { using var restored = backup.Root.CreateSubKey(backup.Path); backup.Before.Write(restored); }
                } catch (Exception cleanup) { failures.Add(cleanup); }
            }
            throw new InvalidOperationException(failures.Count == 1 ? "Registration failed and was restored" :
                "Registration may be partial; inspect the inventory before retrying", new AggregateException(failures));
        }
    }
    private static void Write(RegistryKey root, string path, Dictionary<string, object> values)
    { using var key = root.CreateSubKey(path); foreach (var value in values) key.SetValue(value.Key, value.Value); }
    private static string FullPath(string path)
    {
        if (!HubAttachment.FullyQualified(path) || path.Any(c => char.IsControl(c) || c == '"')) throw new ArgumentException("An absolute path is required");
        var full = Path.GetFullPath(path);
        return full.Length > Path.GetPathRoot(full)!.Length ? full.TrimEnd('\\', '/') : full;
    }
    private static bool SamePath(string first, string second) => string.Equals(first, second, StringComparison.OrdinalIgnoreCase);

    private sealed class Record(HubSelection binding, string directory, string selections, string owner, Version build)
    {
        internal readonly Guid Id = OutputIdentity.ClassId(binding);
        internal readonly string Directory = directory, Selections = selections, Owner = owner;
        internal readonly Version Version = build;
        private readonly HubSelection binding = binding.Copy();
        internal HubSelection Selection => binding.Copy();
        private string Clsid => Id.ToString("B");
        private string ProgId => OutputIdentity.ProgId(binding);
        private string ClassKey => @"Software\Classes\CLSID\" + Clsid;
        private string ProgKey => @"Software\Classes\" + ProgId;
        private string AppKey => @"Software\Classes\AppID\" + Clsid;
        private string ChooserKey => @"Software\ASCOM\" + (binding.DeviceType switch {
            "switch" => "Switch", "safetymonitor" => "SafetyMonitor", "observingconditions" => "ObservingConditions", "focuser" => "Focuser", "rotator" => "Rotator", "filterwheel" => "FilterWheel", "covercalibrator" => "CoverCalibrator", _ => throw new InvalidOperationException()
        }) + @" Drivers\" + ProgId;
        internal string InventoryKey => Inventory + "\\" + Clsid;
        private string Server => Path.Combine(Directory, "hub-ascom", "x64", "Regain.Hub.ASCOM.exe");
        private string Command => $"\"{Server}\" /Embedding --owner-sid {Owner} --bindings \"{Selections}\" --host \"{Path.Combine(Directory, "regain-alpaca.exe")}\"";
        internal int CommandLength => Command.Length;
        private string Name => "PulsarFab regain Hub: " + binding.Label + (binding.Simulated ? " [SIMULATION]" : "");
        internal bool SameIdentity(Record other) => Id == other.Id && Owner == other.Owner &&
            SamePath(Directory, other.Directory) && SamePath(Selections, other.Selections);
        internal string[] TopKeys() => [ClassKey, ProgKey, AppKey, ChooserKey, InventoryKey];
        internal Dictionary<string, Dictionary<string, object>> Entries() => new() {
            [ClassKey] = new() { [""] = Name, ["AppID"] = Clsid },
            [ClassKey + @"\LocalServer32"] = new() { [""] = Command, ["ServerExecutable"] = Server },
            [ClassKey + @"\ProgID"] = new() { [""] = ProgId },
            [ProgKey] = new() { [""] = Name }, [ProgKey + @"\CLSID"] = new() { [""] = Clsid },
            [AppKey] = new() { [""] = Name, ["RunAs"] = "Interactive User" }, [ChooserKey] = new() { [""] = Name }
        };
        internal IEnumerable<(string Path, string Name, object Value)> CriticalValues() {
            var entries = Entries();
            foreach (var path in new[] { ClassKey, ClassKey + @"\LocalServer32", ClassKey + @"\ProgID", ProgKey + @"\CLSID", AppKey })
                foreach (var value in entries[path]) if (path != ClassKey && path != AppKey || value.Key != "") yield return (path, value.Key, value.Value);
        }
        internal Dictionary<string, object> InventoryValues(string phase) => new() {
            ["SchemaVersion"] = 1, ["Phase"] = phase, ["OwnerSid"] = Owner, ["InstallDirectory"] = Directory,
            ["BindingsPath"] = Selections, ["Version"] = Version.ToString(), ["InstanceId"] = binding.InstanceId.ToString("D"),
            ["OutputId"] = binding.OutputId.ToString("D"), ["DeviceType"] = binding.DeviceType,
            ["Label"] = binding.Label, ["Simulated"] = binding.Simulated ? 1 : 0, ["ConfigPath"] = binding.ConfigPath
        };
        internal static Record Read(RegistryKey key) {
            string Text(string name) => key.GetValue(name) as string ?? throw new InvalidOperationException("Invalid hub registration inventory");
            if (!Equals(key.GetValue("SchemaVersion"), 1) || Text("Phase") is not ("pending" or "ready")) throw new InvalidOperationException("Unsupported hub registration inventory");
            var binding = new HubSelection { InstanceId = Guid.ParseExact(Text("InstanceId"), "D"), OutputId = Guid.ParseExact(Text("OutputId"), "D"),
                DeviceType = Text("DeviceType"), Label = Text("Label"), ConfigPath = Text("ConfigPath"), Simulated = Equals(key.GetValue("Simulated"), 1) };
            binding.Validate();
            return new Record(binding, FullPath(Text("InstallDirectory")), FullPath(Text("BindingsPath")),
                new SecurityIdentifier(Text("OwnerSid")).Value, System.Version.Parse(Text("Version")));
        }
    }
    private sealed class SnapshotBudget(int maxKeys, long maxBytes)
    {
        private int keys;
        private long bytes;
        internal void Key(int depth) {
            if (depth > 16 || ++keys > maxKeys) throw new InvalidOperationException("Registration tree exceeds the rollback snapshot limit");
        }
        internal void Value(string name, object value) {
            bytes += 2L * name.Length + (value switch {
                string text => 2L * text.Length,
                string[] texts => texts.Sum(text => 2L * text.Length),
                byte[] data => data.LongLength,
                int => 4L, long => 8L,
                _ => throw new InvalidOperationException("Registration contains an unsupported registry value")
            });
            if (bytes > maxBytes) throw new InvalidOperationException("Registration tree exceeds the rollback snapshot limit");
        }
    }
    private sealed class Tree
    {
        private byte[] security = [];
        private readonly Dictionary<string, (object Value, RegistryValueKind Kind)> values = new();
        private readonly Dictionary<string, Tree> children = new();
        internal static Tree Read(RegistryKey key, SnapshotBudget budget, int depth) {
            budget.Key(depth);
            var tree = new Tree();
            tree.security = ReadSecurity(key);
            budget.Value("", tree.security);
            foreach (var name in key.GetValueNames()) {
                var value = key.GetValue(name, null, RegistryValueOptions.DoNotExpandEnvironmentNames)
                    ?? throw new InvalidOperationException("Registration changed during snapshot");
                budget.Value(name, value);
                tree.values.Add(name, (value, key.GetValueKind(name)));
            }
            foreach (var name in key.GetSubKeyNames()) {
                budget.Value(name, "");
                using var child = key.OpenSubKey(name) ?? throw new InvalidOperationException("Registration changed during snapshot");
                tree.children.Add(name, Read(child, budget, depth + 1));
            }
            return tree;
        }
        internal void Write(RegistryKey key) {
            foreach (var value in values) key.SetValue(value.Key, value.Value.Value, value.Value.Kind);
            foreach (var child in children) { using var target = key.CreateSubKey(child.Key); child.Value.Write(target); }
            RestoreSecurity(key, security);
        }
    }
    // Preserve owner/group/DACL when rollback recreates a tree, without reading
    // SACLs or requiring audit privileges. Default inherited ACLs are not a
    // faithful restoration of a previously customized registration key.
    [DllImport("advapi32.dll")] private static extern int RegGetKeySecurity(IntPtr key, uint information, byte[]? descriptor, ref uint length);
    [DllImport("advapi32.dll")] private static extern int RegSetKeySecurity(IntPtr key, uint information, byte[] descriptor);
    [DllImport("advapi32.dll", CharSet = CharSet.Unicode)] private static extern int RegOpenKeyEx(IntPtr key, string subkey,
        uint options, uint access, out Microsoft.Win32.SafeHandles.SafeRegistryHandle result);
    internal static byte[] ReadSecurity(RegistryKey key)
    {
        uint size = 0;
        var status = RegGetKeySecurity(key.Handle.DangerousGetHandle(), 7, null, ref size);
        if (status != 122 || size is 0 or > 65536) throw new InvalidOperationException("Registry security could not be captured");
        var bytes = new byte[size];
        status = RegGetKeySecurity(key.Handle.DangerousGetHandle(), 7, bytes, ref size);
        if (status != 0) throw new System.ComponentModel.Win32Exception(status);
        return bytes;
    }
    internal static void RestoreSecurity(RegistryKey key, byte[] descriptor)
    {
        // The self-relative descriptor header stores Control in bytes 2/3.
        // Preserve DACL inheritance protection as well as its actual ACEs.
        var protectedDacl = (descriptor[3] & 0x10) != 0; // SE_DACL_PROTECTED
        // RegistryKey's ordinary read/write handle lacks WRITE_DAC/WRITE_OWNER.
        // Reopen the same key with the required security rights; this does not
        // enable privileges or broaden any ACL.
        var status = RegOpenKeyEx(key.Handle.DangerousGetHandle(), "", 0, 0xE0000, out var securityKey);
        using (securityKey) {
            if (status == 0) status = RegSetKeySecurity(securityKey.DangerousGetHandle(), 7u | (protectedDacl ? 0x80000000u : 0x20000000u), descriptor);
        }
        if (status != 0) throw new System.ComponentModel.Win32Exception(status);
    }
}
