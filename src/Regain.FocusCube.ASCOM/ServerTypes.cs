global using Driver = Regain.FocusCube.Driver;
namespace Regain.SerialServer;
internal static class ServerConfiguration {
    public static Type[] Drivers => [typeof(Driver), typeof(Regain.FocusCube.FalconDriver)];
    public static void Shutdown() { Driver.SharedDevice.Session.Dispose(); Regain.FocusCube.FalconDriver.SharedDevice.Session.Dispose(); }
}
