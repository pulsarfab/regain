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
public sealed class Factory : IClassFactory {
    public int CreateInstance(IntPtr outer, ref Guid iid, out IntPtr result) {
        Program.Log("Factory CreateInstance " + iid);
        result=IntPtr.Zero;
        if(outer!=IntPtr.Zero) return unchecked((int)0x80040110);
        try { var unknown=Marshal.GetIUnknownForObject(new Driver()); try { return Marshal.QueryInterface(unknown,ref iid,out result); } finally { Marshal.Release(unknown); } }
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
        uint cookie=0;
        try {
            if(args.Contains("/setup")) {
                object driver=Activator.CreateInstance(Type.GetTypeFromProgID(((ProgIdAttribute)Attribute.GetCustomAttribute(typeof(Driver),typeof(ProgIdAttribute))).Value,true));
                try { driver.GetType().InvokeMember("SetupDialog",System.Reflection.BindingFlags.InvokeMethod,null,driver,null); }
                finally { Marshal.FinalReleaseComObject(driver); }
                return 0;
            }
            var clsid=typeof(Driver).GUID;
            // Private CLSID only for isolated test fixtures; production activation uses the fixed CLSID.
            int test=Array.IndexOf(args,"/test-clsid");
            if(test>=0) clsid=Guid.Parse(args[test+1]);
            Log($"Starting COM server {clsid}; PID {System.Diagnostics.Process.GetCurrentProcess().Id}; session {System.Diagnostics.Process.GetCurrentProcess().SessionId}");
            var app=new Application { ShutdownMode=ShutdownMode.OnExplicitShutdown };
            var factory=new Factory();
            // Register after WPF has initialized its dispatcher/COM apartment.
            app.Dispatcher.BeginInvoke(new Action(() => {
                int registered=CoRegisterClassObject(ref clsid,factory,4,5,out cookie);
                Log($"RegisterClassObject: 0x{registered:X8}");
                Marshal.ThrowExceptionForHR(registered);
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
            timer.Start(); app.Run(); timer.Stop(); GC.KeepAlive(factory); return 0;
        } catch(Exception e) {
            Log(e.ToString()); return 1;
        } finally { if(cookie!=0) CoRevokeClassObject(cookie); Driver.SharedDevice.Session.Dispose(); }
    }
}
