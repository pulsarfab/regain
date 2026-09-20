using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;

internal static class InUseCheck
{
    [StructLayout(LayoutKind.Sequential)]
    internal struct UniqueProcess
    {
        public int Id;
        public System.Runtime.InteropServices.ComTypes.FILETIME Started;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    internal struct ProcessInfo
    {
        public UniqueProcess Process;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string Name;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 64)] public string Service;
        public uint Type, Status, Session;
        [MarshalAs(UnmanagedType.Bool)] public bool Restartable;
    }

    [DllImport("rstrtmgr.dll", CharSet = CharSet.Unicode)]
    private static extern int RmStartSession(out uint session, uint flags, StringBuilder key);
    [DllImport("rstrtmgr.dll", CharSet = CharSet.Unicode)]
    private static extern int RmRegisterResources(uint session, uint fileCount, string[] files,
        uint processCount, IntPtr processes, uint serviceCount, IntPtr services);
    [DllImport("rstrtmgr.dll")]
    private static extern int RmGetList(uint session, out uint needed, ref uint count,
        [In, Out] ProcessInfo[]? processes, ref uint reasons);
    [DllImport("rstrtmgr.dll")]
    private static extern int RmEndSession(uint session);

    // Query only: never shut down camera clients or a capture in progress.
    public static string[] Find(string directory)
    {
        var files = new[] { "ZwoGain.ASCOM.dll", "ZwoGain.Rotator.dll", "zwogain-caa.exe", "zwogain-accessories.exe", "zwogain-ofp2.exe", "zwogain-camera.exe", "zwogain-alpaca.exe", "zwogain-host.exe", "zwogain-direct.exe" }
            .Select(name => Path.Combine(Path.GetFullPath(directory), name)).Where(File.Exists).ToArray();
        if (files.Length == 0) return Array.Empty<string>();
        int error = RmStartSession(out uint session, 0, new StringBuilder(33));
        if (error != 0) throw new Win32Exception(error);
        try {
            error = RmRegisterResources(session, (uint)files.Length, files, 0, IntPtr.Zero, 0, IntPtr.Zero);
            if (error != 0) throw new Win32Exception(error);
            uint count = 0, reasons = 0;
            ProcessInfo[]? processes = null;
            for (int attempt = 0; attempt < 5; attempt++) {
                error = RmGetList(session, out uint needed, ref count, processes, ref reasons);
                if (error == 0) return (processes ?? Array.Empty<ProcessInfo>()).Take((int)count)
                    .Where(p => p.Process.Id != Process.GetCurrentProcess().Id)
                    .Select(p => $"{p.Name} (PID {p.Process.Id})").ToArray();
                if (error != 234) throw new Win32Exception(error);
                count = needed;
                processes = new ProcessInfo[count];
            }
            throw new IOException("The running application list kept changing. Please retry.");
        } finally { RmEndSession(session); }
    }
}
