using System.IO;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Threading;

namespace Regain.SerialServer;

internal static class Program {
    internal static void Track(object driver) => LocalComServer.Track(driver);
    internal static void Log(string message) {
        try {
            var dir=Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),"Regain","ASCOM");
            Directory.CreateDirectory(dir);
            var path=Path.Combine(dir,typeof(Driver).Assembly.GetName().Name + ".log");
            if(File.Exists(path) && new FileInfo(path).Length>2_000_000) File.WriteAllText(path,"");
            File.AppendAllText(path,DateTimeOffset.Now.ToString("O")+" "+message+Environment.NewLine);
        } catch { }
    }
    [STAThread] private static int Main(string[] args) {
        try {
            if(args.Contains("/setup")) {
                int setup=Array.IndexOf(args,"/setup");
                string progid=setup+1<args.Length ? args[setup+1] : ((ProgIdAttribute)Attribute.GetCustomAttribute(typeof(Driver),typeof(ProgIdAttribute))).Value;
                if (!ServerConfiguration.Drivers.Any(t => ((ProgIdAttribute)Attribute.GetCustomAttribute(t,typeof(ProgIdAttribute))).Value == progid)) throw new ArgumentException("Unknown server driver");
                object driver=Activator.CreateInstance(Type.GetTypeFromProgID(progid,true));
                try { driver.GetType().InvokeMember("SetupDialog",System.Reflection.BindingFlags.InvokeMethod,null,driver,null); }
                finally { Marshal.FinalReleaseComObject(driver); }
                return 0;
            }
            var driverTypes=ServerConfiguration.Drivers;
            int chosen=Array.IndexOf(args,"/test-driver");
            if(chosen>=0) driverTypes=driverTypes.Where(t=>((ProgIdAttribute)Attribute.GetCustomAttribute(t,typeof(ProgIdAttribute))).Value==args[chosen+1]).ToArray();
            if(driverTypes.Length==0) throw new ArgumentException("Unknown test driver");
            var clsid=driverTypes[0].GUID;
            // Private CLSID only for isolated test fixtures; production activation uses the fixed CLSID.
            int test=Array.IndexOf(args,"/test-clsid");
            if(test>=0) {clsid=Guid.Parse(args[test+1]);driverTypes=driverTypes.Take(1).ToArray();}
            var classes=driverTypes.Select(type=>new ServerClass(test>=0 ? clsid : type.GUID,type,()=>Activator.CreateInstance(type))).ToArray();
            int ready=Array.IndexOf(args,"/test-ready");
            return LocalComServer.Run(classes,ServerConfiguration.Shutdown,Log,test>=0 && ready>=0 ? args[ready+1] : null);
        } catch(Exception e) {
            Log(e.ToString()); return 1;
        }
    }
}
