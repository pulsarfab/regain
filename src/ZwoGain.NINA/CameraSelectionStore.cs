using System.Text.Json;
using ZwoGain.Core;

namespace ZwoGain.NINA;

internal sealed record CameraSelection(CameraDescriptor Camera, string? Serial = null);

internal sealed class CameraSelectionStore(string path)
{
    private readonly object sync = new();
    public CameraSelection? Load()
    {
        lock (sync)
            return File.Exists(path) ? JsonSerializer.Deserialize<CameraSelection>(File.ReadAllText(path)) : null;
    }
    public void Save(CameraSelection selection)
    {
        lock (sync)
        {
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            string temp = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
            try
            {
                File.WriteAllText(temp, JsonSerializer.Serialize(selection, new JsonSerializerOptions { WriteIndented = true }));
                File.Move(temp, path, true);
            }
            finally { if (File.Exists(temp)) File.Delete(temp); }
        }
    }
    public void RememberSerial(CameraSelection expected, string? serial)
    {
        if (serial is null) return;
        lock (sync)
        {
            var current = Load();
            // Setup may have selected a different camera while connection was in progress.
            if (current?.Camera.Name == expected.Camera.Name && current.Serial == expected.Serial)
                Save(current with { Serial = serial });
        }
    }
}
