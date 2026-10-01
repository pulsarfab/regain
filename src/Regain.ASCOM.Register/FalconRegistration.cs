using Microsoft.Win32;

internal static class FalconRegistration
{
    internal const string ProgId = "ASCOM.PulsarFab.Regain.FalconV2.Rotator";
    internal const string Clsid = "{D2F1A72E-1038-4BFA-BDF1-F1ED7BD3F67B}";
    internal static void Register(RegistryKey root, string directory, bool remove)
    {
        string server = Path.Combine(directory,"Regain.Pegasus.ASCOM.exe");
        string command = "\"" + server + "\" /Embedding";
        string clsid = @"Software\Classes\CLSID\" + Clsid;
        string progid = @"Software\Classes\" + ProgId;
        string chooser = @"Software\ASCOM\Rotator Drivers\" + ProgId;
        if(remove) {
            using var installed=root.OpenSubKey(clsid+@"\LocalServer32");
            if(installed is not null && !string.Equals(installed.GetValue(null) as string,command,StringComparison.OrdinalIgnoreCase))
                throw new InvalidOperationException("Unregister Falcon V2 from its currently installed directory.");
            if(installed is null) return;
            root.DeleteSubKeyTree(clsid,false); root.DeleteSubKeyTree(progid,false); root.DeleteSubKeyTree(chooser,false);
        } else {
            if(!File.Exists(server)) throw new FileNotFoundException("Falcon V2 shared server is missing",server);
            using(var key=root.CreateSubKey(clsid)) { key.SetValue(null,"PulsarFab regain Pegasus Falcon V2"); key.SetValue("AppID",FocusCubeRegistration.Clsid); }
            using(var key=root.CreateSubKey(clsid+@"\LocalServer32")) key.SetValue(null,command);
            using(var key=root.CreateSubKey(clsid+@"\ProgID")) key.SetValue(null,ProgId);
            using(var key=root.CreateSubKey(progid+@"\CLSID")) key.SetValue(null,Clsid);
            using(var key=root.CreateSubKey(chooser)) key.SetValue(null,"PulsarFab regain Pegasus Falcon V2");
        }
    }
}
