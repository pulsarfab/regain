global using Driver = Regain.Ofp2.Driver;
namespace Regain.SerialServer;
internal static class ServerConfiguration {
    public static Type[] Drivers => [typeof(Driver)];
    public static void Shutdown() { Driver.SharedDevice.Session.Dispose();  }
}
