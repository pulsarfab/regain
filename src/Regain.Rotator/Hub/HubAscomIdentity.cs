using System.Security.Cryptography;
using System.Text;

namespace Regain.Hub.ASCOM;

/// Stable bound identities shared by registration and the executable's catalogue.
/// Reordering, renaming, paths and chooser metadata never change these IDs.
public static class OutputIdentity
{
    public static Guid ClassId(HubSelection binding)
    {
        if (binding.InstanceId == Guid.Empty || binding.OutputId == Guid.Empty || !HubSelection.Types.Contains(binding.DeviceType))
            throw new ArgumentException("A supported hub output identity is required");
        var space = Guid.Parse("6ba7b811-9dad-11d1-80b4-00c04fd430c8").ToByteArray();
        NetworkOrder(space);
        var name = Encoding.UTF8.GetBytes($"https://pulsarfab.com/regain/ascom-hub/output/{binding.InstanceId:D}/{binding.OutputId:D}/{binding.DeviceType}");
        using var sha1 = SHA1.Create();
        var bytes = sha1.ComputeHash(space.Concat(name).ToArray()).Take(16).ToArray();
        bytes[6] = (byte)((bytes[6] & 0x0f) | 0x50); bytes[8] = (byte)((bytes[8] & 0x3f) | 0x80);
        NetworkOrder(bytes);
        return new Guid(bytes);
    }
    private static void NetworkOrder(byte[] bytes)
    { Array.Reverse(bytes, 0, 4); Array.Reverse(bytes, 4, 2); Array.Reverse(bytes, 6, 2); }
    public static string ProgId(HubSelection binding) => "Rgn.H" + (binding.DeviceType switch {
        "switch" => "S", "safetymonitor" => "M", "observingconditions" => "W", "focuser" => "F", "rotator" => "R", "filterwheel" => "L", "covercalibrator" => "C", "camera" => "A", _ => throw new ArgumentException("Unknown hub output class")
    }) + "." + ClassId(binding).ToString("N"); // 39 characters: COM's maximum.
}
