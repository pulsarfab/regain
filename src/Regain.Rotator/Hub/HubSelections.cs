using System.IO;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Regain.Hub;

/// Frontend bindings contain identities only. Source parameters and policies
/// remain in the host configuration; changing a path cannot retarget an ID.
public sealed class HubSelection
{
    public string ConfigPath { get; set; } = "";
    public Guid InstanceId { get; set; }
    public Guid OutputId { get; set; }
    public string DeviceType { get; set; } = "";
    public string Label { get; set; } = "";
    public bool Simulated { get; set; }
    [JsonIgnore] public string Id => $"PulsarFab.Regain.Hub.{InstanceId:D}.{OutputId:D}";
    internal void Validate()
    {
        if (ConfigPath is null || !HubAttachment.FullyQualified(ConfigPath) || ConfigPath.Length > 32768 || InstanceId == Guid.Empty ||
            OutputId == Guid.Empty || !Types.Contains(DeviceType, StringComparer.Ordinal) || Label is null || Label.Length is 0 or > 400)
            throw new InvalidOperationException("Invalid saved hub output selection");
    }
    public HubSelection Copy() => new() { ConfigPath = ConfigPath, InstanceId = InstanceId, OutputId = OutputId,
        DeviceType = DeviceType, Label = Label, Simulated = Simulated };
    public static IReadOnlyList<string> Types { get; } = Array.AsReadOnly(new[] { "switch", "safetymonitor", "observingconditions" });
}
public sealed class HubSelections
{
    public int SchemaVersion { get; set; } = 1;
    public Guid Revision { get; set; }
    public HubSelection[] Bindings { get; set; } = [];
}
public sealed class HubSelectionStore(string path)
{
    private static readonly JsonSerializerOptions options = new() {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase, WriteIndented = true,
        UnmappedMemberHandling = JsonUnmappedMemberHandling.Disallow
    };
    public string Path { get; } = System.IO.Path.GetFullPath(path);
    public HubSelections Load()
    {
        if (!File.Exists(Path)) return new();
        using var file = new FileStream(Path, FileMode.Open, FileAccess.Read, FileShare.Read | FileShare.Delete);
        if (file.Length > 512 * 1024) throw new InvalidOperationException("Saved hub selections are too large");
        using var bounded = new MemoryStream();
        var bytes = new byte[4096];
        int count;
        while ((count = file.Read(bytes, 0, bytes.Length)) != 0) {
            if (bounded.Length + count > 512 * 1024) throw new InvalidOperationException("Saved hub selections are too large");
            bounded.Write(bytes, 0, count);
        }
        var content = bounded.ToArray();
        HubWire.Parse(content); // Reject duplicate keys as well as unknown members.
        var saved = JsonSerializer.Deserialize<HubSelections>(content, options) ?? throw new InvalidOperationException("Saved hub selections are missing");
        if (saved.SchemaVersion != 1 || saved.Revision == Guid.Empty || saved.Bindings is null || saved.Bindings.Length > 64)
            throw new InvalidOperationException("Unsupported or invalid saved hub selections");
        foreach (var binding in saved.Bindings) {
            if (binding is null) throw new InvalidOperationException("Invalid saved hub output selection");
            binding.Validate();
        }
        if (saved.Bindings.Select(b => b.Id).Distinct(StringComparer.Ordinal).Count() != saved.Bindings.Length)
            throw new InvalidOperationException("Duplicate saved hub output identities");
        return saved;
    }
    public HubSelections Save(HubSelection selection, Guid expectedRevision)
    {
        selection = selection.Copy();
        selection.Validate();
        Directory.CreateDirectory(System.IO.Path.GetDirectoryName(Path)!);
        // A persistent OS lock protects competing native frontends. It is not
        // removed, and never establishes ownership of equipment or the hub.
        using var file = new FileStream(Path + ".lock", FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.ReadWrite);
        try { file.Lock(0, 1); } catch (IOException) { throw new InvalidOperationException("Another frontend is saving hub selections"); }
        try {
            var current = Load();
            if (current.Revision != expectedRevision) throw new InvalidOperationException("Hub selections changed; reload before saving");
            var next = current.Bindings.Where(b => b.Id != selection.Id).Append(selection).ToArray();
            if (next.Length > 64) throw new InvalidOperationException("Hub selection limit reached");
            var result = new HubSelections { Revision = Guid.NewGuid(), Bindings = next };
            var bytes = JsonSerializer.SerializeToUtf8Bytes(result, options);
            if (bytes.Length > 512 * 1024) throw new InvalidOperationException("Saved hub selections are too large");
            var temporary = Path + "." + Guid.NewGuid().ToString("N") + ".tmp";
            try {
                using (var staged = new FileStream(temporary, FileMode.CreateNew, FileAccess.Write, FileShare.None)) {
                    staged.Write(bytes, 0, bytes.Length); staged.Flush(true);
                }
                if (File.Exists(Path)) File.Replace(temporary, Path, null); else File.Move(temporary, Path);
            } finally { if (File.Exists(temporary)) File.Delete(temporary); }
            return result;
        } finally { file.Unlock(0, 1); }
    }
}
