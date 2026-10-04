using System;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
using System.Text;
using TYPEATTR = System.Runtime.InteropServices.ComTypes.TYPEATTR;
using FUNCDESC = System.Runtime.InteropServices.ComTypes.FUNCDESC;

// Read-only diagnostics after a failed assertion. Never substitutes a value,
// retries the assertion, connects a device or changes COM registration.
public static class ComTestDiagnostics
{
    [ComImport, Guid("00020400-0000-0000-C000-000000000046"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IDispatchProbe
    {
        [PreserveSig] int GetTypeInfoCount(out uint count);
        [PreserveSig] int GetTypeInfo(uint index, uint lcid, out ITypeInfo info);
        [PreserveSig] int GetIDsOfNames(ref Guid iid,
            [MarshalAs(UnmanagedType.LPArray, ArraySubType = UnmanagedType.LPWStr)] string[] names,
            uint count, uint lcid, [Out, MarshalAs(UnmanagedType.LPArray)] int[] ids);
    }

    public static string Read(object device)
    {
        var report = new StringBuilder();
        report.AppendLine("COM diagnostic client bits=" + (IntPtr.Size * 8));
        try
        {
            var dispatch = (IDispatchProbe)device;
            uint count;
            int hr = dispatch.GetTypeInfoCount(out count);
            report.AppendLine("GetTypeInfoCount HRESULT=0x" + hr.ToString("X8") + " count=" + count);
            foreach (string name in new[] { "Name", "InterfaceVersion", "Connected", "Description" })
            {
                var iid = Guid.Empty;
                var ids = new int[1];
                hr = dispatch.GetIDsOfNames(ref iid, new[] { name }, 1, 0, ids);
                report.AppendLine("GetIDsOfNames " + name + " HRESULT=0x" + hr.ToString("X8") + " DISPID=" + ids[0]);
            }
            ITypeInfo info;
            hr = dispatch.GetTypeInfo(0, 0, out info);
            report.AppendLine("GetTypeInfo HRESULT=0x" + hr.ToString("X8"));
            if (hr >= 0 && info != null)
            {
                IntPtr attributes = IntPtr.Zero;
                try
                {
                    info.GetTypeAttr(out attributes);
                    var type = (TYPEATTR)Marshal.PtrToStructure(attributes, typeof(TYPEATTR));
                    report.AppendLine("TypeInfo GUID=" + type.guid + " kind=" + type.typekind + " functions=" + type.cFuncs);
                    for (int i = 0; i < type.cFuncs; i++)
                    {
                        IntPtr description = IntPtr.Zero;
                        try
                        {
                            info.GetFuncDesc(i, out description);
                            var function = (FUNCDESC)Marshal.PtrToStructure(description, typeof(FUNCDESC));
                            var names = new string[1];
                            int found;
                            info.GetNames(function.memid, names, 1, out found);
                            report.AppendLine("Member " + function.memid + " " + function.invkind + " " + (found > 0 ? names[0] : "<unnamed>"));
                        }
                        finally { if (description != IntPtr.Zero) info.ReleaseFuncDesc(description); }
                    }
                }
                finally
                {
                    if (attributes != IntPtr.Zero) info.ReleaseTypeAttr(attributes);
                    Marshal.ReleaseComObject(info);
                }
            }
        }
        catch (Exception e) { report.AppendLine("Diagnostic error: " + e); }
        return report.ToString();
    }
}
