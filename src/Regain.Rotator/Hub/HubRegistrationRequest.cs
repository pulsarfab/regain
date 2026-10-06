using System.Diagnostics;
using System.IO;

namespace Regain.Hub.ASCOM;

public sealed class HubRegisteredOutput(Guid classId, HubSelection selection, string directory,
    string bindingsPath, string ownerSid, Version version, string phase)
{
    public Guid ClassId { get; } = classId;
    public HubSelection Selection { get; } = selection.Copy();
    public string Directory { get; } = directory;
    public string BindingsPath { get; } = bindingsPath;
    public string OwnerSid { get; } = ownerSid;
    public Version Version { get; } = version;
    public string Phase { get; } = phase;
}

/// Immutable action reviewed by setup; elevation uses the existing installed
/// helper. Unknown completion never retries or kills the privileged operation.
public sealed class HubRegistrationRequest
{
    public Guid ClassId { get; }
    public string Arguments { get; }
    private HubRegistrationRequest(Guid classId, IEnumerable<string> args)
    { ClassId = classId; Arguments = string.Join(" ", args.Select(Quote)); }
    public static HubRegistrationRequest Register(HubSelectionStore store, Guid revision, HubSelection binding, string owner)
        => new(OutputIdentity.ClassId(binding), ["/hubregister", store.Path, revision.ToString("D"),
            binding.InstanceId.ToString("D"), binding.OutputId.ToString("D"), owner]);
    public static HubRegistrationRequest Remove(Guid classId, string owner)
        => new(classId, ["/hubunregister", classId.ToString("D"), owner]);
    private static string Quote(string value)
    {
        if (value.Any(c => char.IsControl(c) || c == '"')) throw new ArgumentException("Invalid registration argument");
        // Windows quoted arguments double trailing backslashes. Embedded quotes
        // are forbidden here; no shell command construction is involved.
        return "\"" + value + new string('\\', value.Length - value.TrimEnd('\\').Length) + "\"";
    }
    public async Task<int> RunElevated(string directory)
    {
        var executable = Path.Combine(directory, "Regain.ASCOM.Register.exe");
        if (!HubAttachment.FullyQualified(executable) || !File.Exists(executable)) throw new InvalidOperationException("Installed registration helper is missing");
        using var child = Process.Start(new ProcessStartInfo(executable, Arguments) {
            UseShellExecute = true, Verb = "runas", WindowStyle = ProcessWindowStyle.Hidden,
            WorkingDirectory = directory
        }) ?? throw new InvalidOperationException("Registration helper did not start");
        if (!await Task.Run(() => child.WaitForExit(60000)).ConfigureAwait(false))
            throw new TimeoutException("Registration completion is unknown; reload inventory before another action");
        return child.ExitCode;
    }
}
