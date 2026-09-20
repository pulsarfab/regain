using System.IO;

namespace Regain.Rotator;

/// Stable profile contents and identities survive the product rename.
public static class RegainPaths
{
    public static string? EnvironmentVariable(string name) =>
        Environment.GetEnvironmentVariable(name) ?? Environment.GetEnvironmentVariable(name.Replace("REGAIN_", "ZWOGAIN_"));

    public static string ActionName(string name) => name.ToLowerInvariant().Replace("zwogain.", "regain.");

    public static string Profile(string relativePath, string? localAppData = null)
    {
        if (Path.IsPathRooted(relativePath) || relativePath.Split('/', '\\').Contains(".."))
            throw new ArgumentException("Profile path must stay inside its settings directory", nameof(relativePath));
        string root = localAppData ?? Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
        string target = Path.Combine(root, "Regain", relativePath);
        string legacy = Path.Combine(root, "ZwoGain", relativePath);
        if (!File.Exists(target) && File.Exists(legacy)) {
            Directory.CreateDirectory(Path.GetDirectoryName(target)!);
            string temporary = target + "." + Guid.NewGuid().ToString("N") + ".tmp";
            try {
                File.Copy(legacy, temporary);
                try { File.Move(temporary, target); }
                catch (IOException) when (File.Exists(target)) { } // Another frontend migrated first.
            } finally { if (File.Exists(temporary)) File.Delete(temporary); }
        }
        return target;
    }
}
