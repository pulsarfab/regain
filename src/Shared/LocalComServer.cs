using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Threading;

namespace Regain.SerialServer;

[ComImport, ComVisible(false), Guid("00000001-0000-0000-C000-000000000046"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IClassFactory {
    [PreserveSig] int CreateInstance(IntPtr outer, ref Guid iid, out IntPtr result);
    [PreserveSig] int LockServer([MarshalAs(UnmanagedType.Bool)] bool locked);
}
internal sealed class ServerClass(Guid id, Type type, Func<object> create) {
    internal Guid Id { get; } = id;
    internal Type Type { get; } = type;
    internal Func<object> Create { get; } = create;
}
[ComVisible(true), ClassInterface(ClassInterfaceType.None)]
public sealed class Factory : IClassFactory {
    private readonly Func<object> create;
    internal Factory(Func<object> create) { this.create = create; }
    public int CreateInstance(IntPtr outer, ref Guid iid, out IntPtr result) {
        result = IntPtr.Zero;
        if (outer != IntPtr.Zero) return unchecked((int)0x80040110);
        object? driver = null;
        try {
            driver = create();
            var unknown = Marshal.GetIUnknownForObject(driver);
            try {
                var status = Marshal.QueryInterface(unknown, ref iid, out result);
                if (status < 0) (driver as IDisposable)?.Dispose();
                else LocalComServer.Track(driver);
                return status;
            } finally { Marshal.Release(unknown); }
        } catch (Exception error) { (driver as IDisposable)?.Dispose(); return Marshal.GetHRForException(error); }
    }
    public int LockServer(bool locked) { LocalComServer.ChangeLock(locked); return 0; }
}
/// Shared pumping STA, metadata preparation, class factories and COM lifetime.
internal static class LocalComServer {
    [DllImport("ole32.dll")] private static extern int CoRegisterClassObject(ref Guid clsid, [MarshalAs(UnmanagedType.IUnknown)] object factory, uint context, uint flags, out uint cookie);
    [DllImport("ole32.dll")] private static extern int CoRevokeClassObject(uint cookie);
    [DllImport("ole32.dll")] private static extern int CoResumeClassObjects();
    [DllImport("ole32.dll")] private static extern int CoSuspendClassObjects();
    private static readonly List<WeakReference> objects = [];
    private static int locks;
    internal static void ChangeLock(bool locked) { if (locked) Interlocked.Increment(ref locks); else Interlocked.Decrement(ref locks); }
    internal static void Track(object driver) {
        lock (objects) if (!objects.Any(w => ReferenceEquals(w.Target, driver))) objects.Add(new WeakReference(driver));
    }
    private static IntPtr PrepareDispatchMetadata(Type driverType) {
        var contract = ((ComDefaultInterfaceAttribute?)Attribute.GetCustomAttribute(driverType, typeof(ComDefaultInterfaceAttribute)))?.Value
            ?? throw new InvalidOperationException("Missing default COM interface");
        var pointer = Marshal.GetITypeInfoForType(contract);
        try {
            var info = (System.Runtime.InteropServices.ComTypes.ITypeInfo)Marshal.GetObjectForIUnknown(pointer);
            try {
                info.GetTypeAttr(out var attribute);
                try {
                    var description = (System.Runtime.InteropServices.ComTypes.TYPEATTR)Marshal.PtrToStructure(attribute, typeof(System.Runtime.InteropServices.ComTypes.TYPEATTR));
                    if (description.guid != contract.GUID) throw new InvalidOperationException("Unexpected dispatch interface");
                } finally { info.ReleaseTypeAttr(attribute); }
                foreach (var name in contract.GetProperties().Select(p => p.Name)
                    .Concat(contract.GetMethods().Where(m => !m.IsSpecialName).Select(m => m.Name)).Distinct())
                    info.GetIDsOfNames([name], 1, new int[1]);
            } finally { Marshal.ReleaseComObject(info); }
            return pointer;
        } catch { Marshal.Release(pointer); throw; }
    }
    internal static int Run(IReadOnlyList<ServerClass> classes, Action shutdown, Action<string> log, string? ready = null) {
        if (classes.Count == 0 || classes.Select(c => c.Id).Distinct().Count() != classes.Count) return 2;
        var cookies = new List<uint>(); var metadata = new List<IntPtr>(); var factories = new List<Factory>();
        var failed = false;
        try {
            var app = new Application { ShutdownMode = ShutdownMode.OnExplicitShutdown };
            app.DispatcherUnhandledException += (_, args) => { failed = true; log("COM server dispatch failed"); args.Handled = true; app.Shutdown(); };
            app.Dispatcher.BeginInvoke(new Action(() => {
                foreach (var type in classes.Select(c => c.Type).Distinct()) metadata.Add(PrepareDispatchMetadata(type));
                foreach (var entry in classes) {
                    var id = entry.Id;
                    var factory = new Factory(entry.Create); factories.Add(factory);
                    Marshal.ThrowExceptionForHR(CoRegisterClassObject(ref id, factory, 4, 5, out var cookie));
                    cookies.Add(cookie);
                }
                Marshal.ThrowExceptionForHR(CoResumeClassObjects());
                if (ready is not null) System.IO.File.WriteAllText(ready, "ready");
            }));
            var idle = DateTime.UtcNow;
            var timer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(5) };
            timer.Tick += (_, _) => {
                GC.Collect(); GC.WaitForPendingFinalizers();
                lock (objects) {
                    objects.RemoveAll(w => !w.IsAlive);
                    if (objects.Count > 0 || Volatile.Read(ref locks) > 0) idle = DateTime.UtcNow;
                    else if (DateTime.UtcNow - idle > TimeSpan.FromSeconds(30)) { CoSuspendClassObjects(); app.Shutdown(); }
                }
            };
            timer.Start(); app.Run(); timer.Stop(); GC.KeepAlive(factories);
            return failed ? 1 : 0;
        } catch { log("COM server startup failed"); return 1; }
        finally {
            foreach (var cookie in cookies) CoRevokeClassObject(cookie);
            foreach (var pointer in metadata) Marshal.Release(pointer);
            shutdown();
        }
    }
}
