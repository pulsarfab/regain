using System.Security.Cryptography;
using System.Text;

namespace Regain.Core;

/// <summary>A durable no-replay fence, held open until the actual vendor operation ends.</summary>
public sealed class MountRecoveryJournal(string directory)
{
    public static string DefaultDirectory => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "PulsarFab", "Regain", "MountRecovery");
    private string PathFor(string mountId) => Path.Combine(directory,
        Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(mountId))) + ".pending");
    public bool Required(string mountId) => File.Exists(PathFor(mountId));
    public Attempt Begin(string mountId) => Open(mountId, FileMode.CreateNew);
    /// <summary>Lock an existing failure fence while issuing stop-only commands; never authorizes recovery motion.</summary>
    public Attempt HoldForAbort(string mountId) => Open(mountId, FileMode.OpenOrCreate);
    private Attempt Open(string mountId, FileMode mode)
    {
        Directory.CreateDirectory(directory);
        string path = PathFor(mountId);
        var stream = new FileStream(path, mode, FileAccess.ReadWrite, FileShare.Delete,
            4096, FileOptions.WriteThrough);
        try {
            if (stream.Length == 0) stream.Write(Encoding.UTF8.GetBytes($"{DateTimeOffset.UtcNow:O}\n{mountId}\n"));
            stream.Flush(flushToDisk: true);
            return new(path, stream);
        } catch { stream.Dispose(); throw; } // Leave the fence if writing it was uncertain.
    }
    public bool ClearAfterInspection(string mountId)
    {
        try {
            if (!Required(mountId)) return true;
            // An active attempt's exclusive handle prevents this open. Delete on
            // close keeps inspection/clearing atomic with respect to new attempts.
            using var marker = new FileStream(PathFor(mountId), FileMode.Open, FileAccess.ReadWrite,
                FileShare.None, 4096, FileOptions.DeleteOnClose);
            return true;
        } catch (IOException) { return false; }
        catch (UnauthorizedAccessException) { return false; }
    }
    public sealed class Attempt : IDisposable
    {
        private readonly string path;
        private readonly FileStream stream;
        internal Attempt(string path, FileStream stream) { this.path = path; this.stream = stream; }
        public void Complete() { File.Delete(path); stream.Dispose(); }
        public void Dispose() => stream.Dispose(); // Failure/cancellation retains the marker.
    }
}
