using System.Diagnostics;
using System.IO;
using System.IO.Pipes;
using System.Runtime.InteropServices;
using System.Security.AccessControl;
using System.Security.Principal;
using System.Text;
using System.Text.Json;
using Microsoft.Win32.SafeHandles;

namespace Regain.Hub;

public sealed class HubAttachment
{
    public Guid InstanceId { get; }
    public Guid HostInstance { get; }
    public Guid ConfigurationRevision { get; }
    public string Address { get; }
    // A launch candidate, never ownership authority. Do not kill it when a
    // frontend closes. This value exists for attachment diagnostics only.
    public uint? StartedProcessId { get; }
    internal HubAttachment(JsonElement value, Guid expected)
    {
        HubWire.Members(value, "protocolVersion", "instanceId", "hostInstance", "configurationRevision", "transport", "address", "startedProcessId");
        InstanceId = HubWire.Identity(value, "instanceId");
        HostInstance = HubWire.Identity(value, "hostInstance");
        ConfigurationRevision = HubWire.Identity(value, "configurationRevision");
        Address = value.GetProperty("address").GetString() ?? "";
        if (value.GetProperty("protocolVersion").GetInt32() != 1 || InstanceId != expected ||
            value.GetProperty("transport").GetString() != "namedPipe") throw new HubException(HubFailure.Protocol);
        HubPipe.Name(Address);
        if (value.TryGetProperty("startedProcessId", out var pid) && pid.ValueKind != JsonValueKind.Null) {
            var number = pid.GetUInt32();
            if (number == 0) throw new HubException(HubFailure.Protocol);
            StartedProcessId = number;
        }
    }

    /// Uses the existing Rust attachment helper once. Readiness failure never
    /// triggers another launch, kills an owner, or joins a device worker job.
    public static async Task<HubAttachment> AttachAsync(string executable, string configPath,
        string? workerDirectory = null, CancellationToken cancellation = default, Guid? expectedInstance = null)
    {
        AbsoluteFile(executable); AbsoluteFile(configPath);
        if (workerDirectory is not null && (!FullyQualified(workerDirectory) || !Directory.Exists(workerDirectory)))
            throw new HubException(HubFailure.InvalidRequest);
        if (new FileInfo(configPath).Length > 4 * 1024 * 1024) throw new HubException(HubFailure.InvalidRequest);
        Guid instance;
        try {
            // Bound actual reads too: a concurrent replacement cannot turn the
            // preliminary length check into an unbounded allocation.
            using var config = new FileStream(configPath, FileMode.Open, FileAccess.Read, FileShare.Read | FileShare.Delete);
            var bytes = await ReadBounded(config, 4 * 1024 * 1024, cancellation).ConfigureAwait(false);
            try { instance = HubWire.Identity(HubWire.Parse(bytes), "instanceId"); }
            finally { Array.Clear(bytes, 0, bytes.Length); }
        } catch (OperationCanceledException) { throw; }
        catch (Exception) { throw new HubException(HubFailure.InvalidRequest); }

        // A saved binding must reject a changed installation before it starts
        // or attaches to any host. Setup discovery intentionally omits this ID.
        if (expectedInstance.HasValue && instance != expectedInstance.Value) throw new HubException(HubFailure.Protocol);

        var arguments = "--hub-attach --hub-config " + Quote(configPath);
        if (workerDirectory is not null) arguments += " --workers " + Quote(workerDirectory);
        using var process = new Process { StartInfo = new ProcessStartInfo(executable, arguments) {
            UseShellExecute = false, CreateNoWindow = true, WindowStyle = ProcessWindowStyle.Hidden,
            RedirectStandardOutput = true, RedirectStandardError = true,
            WorkingDirectory = Path.GetDirectoryName(executable)!
        } };
        using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        timer.CancelAfter(TimeSpan.FromSeconds(15));
        Task<byte[]>? output = null, errors = null;
        try {
            if (!process.Start()) throw new HubException(HubFailure.Disconnected);
            using var killHelper = timer.Token.Register(() => StopHelper(process));
            output = ReadBounded(process.StandardOutput.BaseStream, 16384, timer.Token);
            errors = ReadBounded(process.StandardError.BaseStream, 16384, timer.Token);
            await Task.WhenAll(output, errors).ConfigureAwait(false);
            // Do not export arbitrary helper/driver stderr, including on error.
            Array.Clear(errors.Result, 0, errors.Result.Length);
            while (!process.HasExited) await Task.Delay(10, timer.Token).ConfigureAwait(false);
            cancellation.ThrowIfCancellationRequested();
            if (timer.IsCancellationRequested) throw new HubException(HubFailure.Timeout);
            if (process.ExitCode != 0) throw new HubException(HubFailure.Disconnected);
            try { return new HubAttachment(HubWire.Parse(output.Result), instance); }
            finally { Array.Clear(output.Result, 0, output.Result.Length); }
        } catch (Exception e) {
            StopHelper(process);
            cancellation.ThrowIfCancellationRequested();
            if (timer.IsCancellationRequested) throw new HubException(HubFailure.Timeout);
            throw e is HubException ? e : new HubException(HubFailure.Disconnected);
        } finally {
            if (output?.Status == TaskStatus.RanToCompletion) Array.Clear(output.Result, 0, output.Result.Length);
            if (errors?.Status == TaskStatus.RanToCompletion) Array.Clear(errors.Result, 0, errors.Result.Length);
        }
    }
    private static void AbsoluteFile(string path)
    {
        if (!FullyQualified(path) || !File.Exists(path)) throw new HubException(HubFailure.InvalidRequest);
    }
    internal static bool FullyQualified(string path) => !string.IsNullOrEmpty(path) &&
        (path.StartsWith(@"\\", StringComparison.Ordinal) || (path.Length >= 3 && char.IsLetter(path[0]) &&
            path[1] == ':' && path[2] is '\\' or '/'));
    private static void StopHelper(Process process)
    {
        // Kill only this helper. Its Rust child may have become the shared owner.
        try { if (!process.HasExited) process.Kill(); } catch { }
        try { process.StandardOutput.Dispose(); } catch { }
        try { process.StandardError.Dispose(); } catch { }
    }
    private static async Task<byte[]> ReadBounded(Stream stream, int maximum, CancellationToken token)
    {
        using var memory = new MemoryStream();
        var chunk = new byte[4096];
        try {
            while (true) {
                var read = await stream.ReadAsync(chunk, 0, chunk.Length, token).ConfigureAwait(false);
                if (read == 0) return memory.ToArray();
                if (read > maximum - memory.Length) throw new HubException(HubFailure.InvalidRequest);
                memory.Write(chunk, 0, read);
            }
        } finally {
            Array.Clear(chunk, 0, chunk.Length);
            if (memory.TryGetBuffer(out var buffer)) Array.Clear(buffer.Array!, buffer.Offset, buffer.Count);
        }
    }
    internal static string Quote(string value)
    {
        if (value.IndexOf('\0') >= 0) throw new HubException(HubFailure.InvalidRequest);
        var result = new StringBuilder("\"");
        var slashes = 0;
        foreach (var c in value) {
            if (c == '\\') { slashes++; continue; }
            if (c == '"') result.Append('\\', slashes * 2 + 1).Append('"');
            else result.Append('\\', slashes).Append(c);
            slashes = 0;
        }
        return result.Append('\\', slashes * 2).Append('"').ToString();
    }
}

public sealed partial class HubClient
{
    public static async Task<HubClient> ConnectAsync(HubAttachment attachment, TimeSpan? deadline = null,
        HubClientLimits? limits = null, CancellationToken cancellation = default)
    {
        var bound = deadline ?? TimeSpan.FromSeconds(10);
        if (bound <= TimeSpan.Zero || bound > TimeSpan.FromSeconds(300)) throw new HubException(HubFailure.InvalidRequest);
        using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        timer.CancelAfter(bound);
        var pipe = new NamedPipeClientStream(".", HubPipe.Name(attachment.Address), PipeDirection.InOut,
            PipeOptions.Asynchronous, TokenImpersonationLevel.Identification, HandleInheritability.None);
        try {
            await pipe.ConnectAsync((int)bound.TotalMilliseconds, timer.Token).ConfigureAwait(false);
            HubPipe.Verify(pipe.SafePipeHandle);
            var client = await FromStreamAsync(pipe, attachment.InstanceId, bound, limits, timer.Token).ConfigureAwait(false);
            if (client.Hello.HostInstance != attachment.HostInstance) {
                client.Dispose(); throw new HubException(HubFailure.Protocol);
            }
            if (timer.IsCancellationRequested) {
                client.Dispose(); cancellation.ThrowIfCancellationRequested();
                throw new HubException(HubFailure.Timeout);
            }
            return client;
        } catch (Exception e) {
            pipe.Dispose(); cancellation.ThrowIfCancellationRequested();
            if (timer.IsCancellationRequested || e is TimeoutException) throw new HubException(HubFailure.Timeout);
            throw e is HubException ? e : new HubException(HubFailure.Protocol);
        }
    }
}

internal static class HubPipe
{
    internal static string Name(string address)
    {
        const string prefix = @"\\.\pipe\PulsarFab.Regain.Hub.";
        if (!address.StartsWith(prefix, StringComparison.Ordinal) || address.Length != prefix.Length + 64 ||
            address.Substring(prefix.Length).Any(c => !(c is >= '0' and <= '9' or >= 'a' and <= 'f')))
            throw new HubException(HubFailure.Protocol);
        return address.Substring(@"\\.\pipe\".Length);
    }
    internal static void Verify(SafePipeHandle pipe)
    {
        IntPtr descriptor = IntPtr.Zero;
        try {
            var status = GetSecurityInfo(pipe, 1 /* SE_FILE_OBJECT */, 5 /* owner + DACL */, out _, out _, out _, out _, out descriptor);
            if (status != 0 || descriptor == IntPtr.Zero) throw new HubException(HubFailure.Protocol);
            var length = GetSecurityDescriptorLength(descriptor);
            if (length == 0 || length > 65536) throw new HubException(HubFailure.Protocol);
            var bytes = new byte[(int)length];
            Marshal.Copy(descriptor, bytes, 0, bytes.Length);
            using var identity = WindowsIdentity.GetCurrent();
            if (identity.User is null) throw new HubException(HubFailure.Protocol);
            VerifyDescriptor(new RawSecurityDescriptor(bytes, 0), identity.User);
        } finally { if (descriptor != IntPtr.Zero) LocalFree(descriptor); }
    }
    internal static void VerifyDescriptor(RawSecurityDescriptor descriptor, SecurityIdentifier user)
    {
        var required = ControlFlags.DiscretionaryAclPresent | ControlFlags.DiscretionaryAclProtected;
        if (descriptor.Owner != user || (descriptor.ControlFlags & required) != required ||
            descriptor.DiscretionaryAcl is not { Count: 1 } acl ||
            acl[0] is not CommonAce ace || ace.AceType != AceType.AccessAllowed || ace.IsCallback || ace.SecurityIdentifier != user)
            throw new HubException(HubFailure.Protocol);
    }
    [DllImport("advapi32.dll")] private static extern uint GetSecurityInfo(SafePipeHandle handle, int objectType, uint info,
        out IntPtr owner, out IntPtr group, out IntPtr dacl, out IntPtr sacl, out IntPtr descriptor);
    [DllImport("advapi32.dll")] private static extern uint GetSecurityDescriptorLength(IntPtr descriptor);
    [DllImport("kernel32.dll")] private static extern IntPtr LocalFree(IntPtr memory);
}
