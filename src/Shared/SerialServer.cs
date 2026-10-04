using System.IO;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Threading;

namespace Regain.SerialServer;

[ComImport, ComVisible(false), Guid("00000001-0000-0000-C000-000000000046"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IClassFactory {
    [PreserveSig] int CreateInstance(IntPtr outer, ref Guid iid, out IntPtr result);
    [PreserveSig] int LockServer([MarshalAs(UnmanagedType.Bool)] bool locked);
}
[ComVisible(true), ClassInterface(ClassInterfaceType.None)]
public sealed class Factory(Type driverType) : IClassFactory {
    public int CreateInstance(IntPtr outer, ref Guid iid, out IntPtr result) {
        Program.Log("Factory CreateInstance " + iid);
        result=IntPtr.Zero;
        if(outer!=IntPtr.Zero) return unchecked((int)0x80040110);
        try { var unknown=Marshal.GetIUnknownForObject(Activator.CreateInstance(driverType)); try { return Marshal.QueryInterface(unknown,ref iid,out result); } finally { Marshal.Release(unknown); } }
        catch(Exception e) { return Marshal.GetHRForException(e); }
    }
    public int LockServer(bool locked) { Program.Locks += locked ? 1 : -1; return 0; }
}
internal static class Program {
    [DllImport("ole32.dll")] private static extern int CoRegisterClassObject(ref Guid clsid, [MarshalAs(UnmanagedType.IUnknown)] object factory, uint context, uint flags, out uint cookie);
    [DllImport("ole32.dll")] private static extern int CoRevokeClassObject(uint cookie);
    [DllImport("ole32.dll")] private static extern int CoResumeClassObjects();
    [DllImport("ole32.dll")] private static extern int CoSuspendClassObjects();
    private static readonly List<WeakReference> Objects=[];
    internal static int Locks;
    // Resolve dispatch metadata on the server STA before publishing any class
    // factories, keeping lazy CLR type-info work out of concurrent first requests.
    // No driver instance or device connection is needed for this preparation.
    private static IntPtr PrepareDispatchMetadata(Type driverType) {
        var declaration=(ComDefaultInterfaceAttribute?)Attribute.GetCustomAttribute(driverType,typeof(ComDefaultInterfaceAttribute));
        var contract=declaration?.Value ?? throw new InvalidOperationException($"{driverType.Name} has no default COM interface");
        var pointer=Marshal.GetITypeInfoForType(contract);
        try {
            var info=(System.Runtime.InteropServices.ComTypes.ITypeInfo)Marshal.GetObjectForIUnknown(pointer);
            try {
                info.GetTypeAttr(out var attribute);
                try {
                    var description=(System.Runtime.InteropServices.ComTypes.TYPEATTR)Marshal.PtrToStructure(attribute,typeof(System.Runtime.InteropServices.ComTypes.TYPEATTR));
                    if(description.guid!=contract.GUID) throw new InvalidOperationException($"Unexpected dispatch interface for {driverType.Name}: {description.guid}");
                } finally { info.ReleaseTypeAttr(attribute); }
                var names=contract.GetProperties().Select(p=>p.Name)
                    .Concat(contract.GetMethods().Where(m=>!m.IsSpecialName).Select(m=>m.Name)).Distinct();
                foreach(var name in names) info.GetIDsOfNames([name],1,new int[1]);
                Log($"Prepared dispatch metadata {driverType.Name}: {contract.GUID}");
            } finally { Marshal.ReleaseComObject(info); }
            // Retain an owned reference until class objects have been revoked.
            return pointer;
        } catch { Marshal.Release(pointer); throw; }
    }
    internal static void Track(object driver) { lock(Objects) Objects.Add(new WeakReference(driver)); }
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
        var cookies=new List<uint>();
        var metadata=new List<IntPtr>();
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
            Log($"Starting COM server {clsid}; PID {System.Diagnostics.Process.GetCurrentProcess().Id}; session {System.Diagnostics.Process.GetCurrentProcess().SessionId}");
            var app=new Application { ShutdownMode=ShutdownMode.OnExplicitShutdown };
            var factories=new List<Factory>();
            // Register after WPF has initialized its dispatcher/COM apartment.
            app.Dispatcher.BeginInvoke(new Action(() => {
                foreach(var type in driverTypes) metadata.Add(PrepareDispatchMetadata(type));
                foreach(var type in driverTypes) {
                    var registration=test>=0 ? clsid : type.GUID;
                    var factory=new Factory(type); factories.Add(factory);
                    int registered=CoRegisterClassObject(ref registration,factory,4,5,out uint cookie);
                    Log($"RegisterClassObject {type.Name}: 0x{registered:X8}");
                    Marshal.ThrowExceptionForHR(registered); cookies.Add(cookie);
                }
                int resumed=CoResumeClassObjects();
                Log($"ResumeClassObjects: 0x{resumed:X8}");
                Marshal.ThrowExceptionForHR(resumed);
                int ready=Array.IndexOf(args,"/test-ready");
                if(test>=0 && ready>=0) File.WriteAllText(args[ready+1],"ready");
            }));
            var idle=DateTime.UtcNow;
            var timer=new DispatcherTimer { Interval=TimeSpan.FromSeconds(5) };
            timer.Tick+=(_,_)=>{
                GC.Collect(); GC.WaitForPendingFinalizers();
                lock(Objects) {
                    Objects.RemoveAll(w=>!w.IsAlive);
                    if(Objects.Count>0 || Locks>0) idle=DateTime.UtcNow;
                    else if(DateTime.UtcNow-idle>TimeSpan.FromSeconds(30)) { CoSuspendClassObjects(); app.Shutdown(); }
                }
            };
            timer.Start(); app.Run(); timer.Stop(); GC.KeepAlive(factories); return 0;
        } catch(Exception e) {
            Log(e.ToString()); return 1;
        } finally {
            foreach(uint cookie in cookies) CoRevokeClassObject(cookie);
            foreach(var pointer in metadata) Marshal.Release(pointer);
            ServerConfiguration.Shutdown();
        }
    }
}
