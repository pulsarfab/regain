using System.Runtime.InteropServices;
using System.Windows.Forms;
using Microsoft.Win32;

[assembly: ComVisible(false)]
namespace ZwoGain.Ascom;

internal static class Program
{
    internal static int Objects, Locks;
    internal static readonly Type[] Cameras = [typeof(Camera1), typeof(Camera2), typeof(Camera3), typeof(Camera4)];
    [MTAThread]
    static int Main(string[] args)
    {
        try
        {
            if (args.Any(a => a.Equals("/regserver", StringComparison.OrdinalIgnoreCase))) { Register(false, args.Contains("--user")); return 0; }
            if (args.Any(a => a.Equals("/unregserver", StringComparison.OrdinalIgnoreCase))) { Register(true, args.Contains("--user")); return 0; }
            int hr = CoInitializeEx(IntPtr.Zero, 0); if (hr < 0) Marshal.ThrowExceptionForHR(hr);
            var factories = new List<ClassFactory>(); var cookies = new List<uint>();
            try
            {
                foreach (var type in Cameras)
                {
                    var factory = new ClassFactory(type); factories.Add(factory); Guid id = type.GUID;
                    Marshal.ThrowExceptionForHR(CoRegisterClassObject(ref id, factory, 4, 1, out uint cookie)); cookies.Add(cookie);
                }
                // COM release makes wrappers collectible; quit when all clients have gone.
                using var idle = new System.Windows.Forms.Timer { Interval = 30000 };
                idle.Tick += (_, _) => { GC.Collect(); GC.WaitForPendingFinalizers(); if (Volatile.Read(ref Objects) == 0 && Volatile.Read(ref Locks) == 0) Application.ExitThread(); };
                idle.Start(); Application.Run(); GC.KeepAlive(factories);
            }
            finally { foreach (uint cookie in cookies) CoRevokeClassObject(cookie); CoUninitialize(); }
            return 0;
        }
        catch (Exception error)
        {
            string dir = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "ZwoGain", "ASCOM");
            try { Directory.CreateDirectory(dir); File.AppendAllText(Path.Combine(dir, "server.log"), DateTime.UtcNow.ToString("O") + " " + error + Environment.NewLine); } catch { }
            Console.Error.WriteLine(error); return 1;
        }
    }
    private static void Register(bool remove, bool user)
    {
        string executable = typeof(Program).Assembly.Location;
        foreach (RegistryView view in new[] { RegistryView.Registry32, RegistryView.Registry64 })
        {
            using var root = RegistryKey.OpenBaseKey(user ? RegistryHive.CurrentUser : RegistryHive.LocalMachine, view);
            for (int slot = 0; slot < Cameras.Length; slot++)
            {
                var type = Cameras[slot]; string progid = "ASCOM.ZWOgain.Camera" + (slot + 1); string clsid = type.GUID.ToString("B");
                string friendly = "ZWOgain Retryable Camera " + (slot + 1);
                string classPath = @"Software\Classes\CLSID\" + clsid;
                if (remove)
                {
                    using var existing = root.OpenSubKey(classPath + @"\LocalServer32");
                    if ((existing?.GetValue(null) as string)?.Contains(executable) != true) continue;
                    root.DeleteSubKeyTree(classPath, false); root.DeleteSubKeyTree(@"Software\Classes\" + progid, false);
                    if (!user) root.DeleteSubKeyTree(@"Software\ASCOM\Camera Drivers\" + progid, false);
                }
                else
                {
                    using var registration = root.CreateSubKey(classPath);
                    registration.SetValue(null, friendly);
                    using (var server = registration.CreateSubKey("LocalServer32")) server.SetValue(null, "\"" + executable + "\" /embedding");
                    using (var name = registration.CreateSubKey("ProgID")) name.SetValue(null, progid);
                    using (var prog = root.CreateSubKey(@"Software\Classes\" + progid + @"\CLSID")) prog.SetValue(null, clsid);
                    if (!user) { using var chooser = root.CreateSubKey(@"Software\ASCOM\Camera Drivers\" + progid); chooser.SetValue(null, friendly); }
                }
            }
        }
    }
    [DllImport("ole32.dll")] private static extern int CoInitializeEx(IntPtr reserved, uint mode);
    [DllImport("ole32.dll")] private static extern void CoUninitialize();
    [DllImport("ole32.dll")] private static extern int CoRegisterClassObject(ref Guid clsid, [MarshalAs(UnmanagedType.Interface)] IClassFactory factory, uint context, uint flags, out uint cookie);
    [DllImport("ole32.dll")] private static extern int CoRevokeClassObject(uint cookie);
}

[ComImport, Guid("00000001-0000-0000-C000-000000000046"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
internal interface IClassFactory
{
    [PreserveSig] int CreateInstance(IntPtr outer, ref Guid iid, out IntPtr result);
    [PreserveSig] int LockServer([MarshalAs(UnmanagedType.Bool)] bool value);
}
[ComVisible(true), ClassInterface(ClassInterfaceType.None)]
internal sealed class ClassFactory(Type type) : IClassFactory
{
    public int CreateInstance(IntPtr outer, ref Guid iid, out IntPtr result)
    {
        result = IntPtr.Zero; if (outer != IntPtr.Zero) return unchecked((int)0x80040110);
        try
        {
            var instance = Activator.CreateInstance(type)!;
            IntPtr unknown = Marshal.GetIUnknownForObject(instance);
            try { return Marshal.QueryInterface(unknown, ref iid, out result); }
            finally { Marshal.Release(unknown); }
        }
        catch (Exception error) { return Marshal.GetHRForException(error); }
    }
    public int LockServer(bool value) { if (value) Interlocked.Increment(ref Program.Locks); else Interlocked.Decrement(ref Program.Locks); return 0; }
}
