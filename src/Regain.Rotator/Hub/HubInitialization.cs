using System.IO;
using System.Text.Json;

namespace Regain.Hub;

public enum HubInitializationState { Idle, Creating, Created, Uncertain, Reading, Existing, Missing }

/// Explicit creation through the Rust persistence path. The selected filename
/// survives unknown completion; reconciliation reads it without starting a host.
public sealed class HubInitialization
{
    private readonly Func<string, CancellationToken, Task<byte[]>> create;
    private readonly SemaphoreSlim operation = new(1, 1);
    public string? Path { get; private set; }
    public HubInitializationState State { get; private set; }
    public Guid? InstanceId { get; private set; }
    public Guid? Revision { get; private set; }
    public bool RequiresReconciliation => State == HubInitializationState.Uncertain;
    public HubInitialization(string executable) : this((path, cancellation) =>
        HubAttachment.RunHelperAsync(executable, "--hub-init --hub-config " + HubAttachment.Quote(path), cancellation)) { }
    internal HubInitialization(Func<string, CancellationToken, Task<byte[]>> create) => this.create = create;

    public async Task CreateAsync(string path, CancellationToken cancellation = default)
    {
        ValidatePath(path);
        if (!await operation.WaitAsync(0, cancellation).ConfigureAwait(false)) throw new HubException(HubFailure.Busy);
        try {
            if (RequiresReconciliation) throw new InvalidOperationException("Read the retained configuration file before another creation");
            cancellation.ThrowIfCancellationRequested();
            Path = path; InstanceId = Revision = null; State = HubInitializationState.Creating;
            try {
                var bytes = await create(path, cancellation).ConfigureAwait(false);
                try {
                    cancellation.ThrowIfCancellationRequested();
                    var result = HubWire.Parse(bytes); ReadIdentity(result);
                    if (result.GetProperty("sources").GetArrayLength() != 0 || result.GetProperty("outputs").GetArrayLength() != 0)
                        throw new HubException(HubFailure.Protocol);
                    State = HubInitializationState.Created;
                } finally { Array.Clear(bytes, 0, bytes.Length); }
            } catch { InstanceId = Revision = null; State = HubInitializationState.Uncertain; throw; }
        } finally { operation.Release(); }
    }
    internal static void ValidatePath(string path)
    {
        if (!HubAttachment.FullyQualified(path) || path.IndexOf('\0') >= 0 ||
            string.IsNullOrEmpty(System.IO.Path.GetFileName(path)) || !Directory.Exists(System.IO.Path.GetDirectoryName(path)))
            throw new HubException(HubFailure.InvalidRequest);
    }

    public async Task ReadAsync(CancellationToken cancellation = default)
    {
        if (!await operation.WaitAsync(0, cancellation).ConfigureAwait(false)) throw new HubException(HubFailure.Busy);
        try {
            if (Path is null) throw new InvalidOperationException("Select a filename through Create first");
            State = HubInitializationState.Reading; InstanceId = Revision = null;
            using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
            timer.CancelAfter(TimeSpan.FromSeconds(15));
            try {
                using var file = new FileStream(Path, FileMode.Open, FileAccess.Read, FileShare.Read | FileShare.Delete, 4096, FileOptions.Asynchronous);
                var bytes = await HubAttachment.ReadBounded(file, 4 * 1024 * 1024, timer.Token).ConfigureAwait(false);
                try { ReadIdentity(HubWire.Parse(bytes)); State = HubInitializationState.Existing; }
                finally { Array.Clear(bytes, 0, bytes.Length); }
            } catch (FileNotFoundException) { State = HubInitializationState.Missing; }
            catch { InstanceId = Revision = null; State = HubInitializationState.Uncertain; throw; }
        } finally { operation.Release(); }
    }
    private void ReadIdentity(JsonElement configuration)
    {
        HubWire.Members(configuration, "schemaVersion", "instanceId", "revision", "sources", "outputs", "identities", "focuserGroups");
        if (configuration.ValueKind != JsonValueKind.Object || configuration.GetProperty("schemaVersion").GetInt32() != 1 ||
            configuration.GetProperty("sources").ValueKind != JsonValueKind.Array || configuration.GetProperty("outputs").ValueKind != JsonValueKind.Array ||
            configuration.TryGetProperty("focuserGroups", out var groups) && groups.ValueKind != JsonValueKind.Array)
            throw new HubException(HubFailure.Protocol);
        // This is bounded file identification, not host validation or permission.
        InstanceId = HubWire.Identity(configuration, "instanceId"); Revision = HubWire.Identity(configuration, "revision");
    }
}
