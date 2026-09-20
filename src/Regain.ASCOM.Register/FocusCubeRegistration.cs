using Microsoft.Win32;

internal static class FocusCubeRegistration
{
    internal const string ProgId = "ASCOM.ZWOgain.FocusCube3.Focuser";
    internal const string Clsid = "{69AB224B-14D2-46A2-A744-0C60593A28B3}";
    internal static void Register(RegistryKey root, string directory, bool remove)
    {
        string server = Path.Combine(directory,"Regain.FocusCube.ASCOM.exe");
        string command = "\"" + server + "\" /Embedding";
        string clsid = @"Software\Classes\CLSID\" + Clsid;
        string progid = @"Software\Classes\" + ProgId;
        string appid = @"Software\Classes\AppID\" + Clsid;
        string appExe = @"Software\Classes\AppID\Regain.FocusCube.ASCOM.exe";
        string chooser = @"Software\ASCOM\Focuser Drivers\" + ProgId;
        if(remove) {
            using var installed=root.OpenSubKey(clsid+@"\LocalServer32");
            if(installed is not null && !string.Equals(installed.GetValue(null) as string,command,StringComparison.OrdinalIgnoreCase))
                throw new InvalidOperationException("Unregister FocusCube3 from its currently installed directory.");
            if(installed is null) return;
            root.DeleteSubKeyTree(clsid,false); root.DeleteSubKeyTree(progid,false); root.DeleteSubKeyTree(chooser,false);
            root.DeleteSubKeyTree(appid,false); root.DeleteSubKeyTree(appExe,false);
        } else {
            if(!File.Exists(server)) throw new FileNotFoundException("FocusCube3 shared server is missing",server);
            // Match the ASCOM LocalServer template: a single interactive identity
            // also shares the server between elevated and ordinary clients.
            using(var key=root.CreateSubKey(appid)) { key.SetValue(null,"PulsarFab regain FocusCube3 shared server"); key.SetValue("RunAs","Interactive User"); }
            using(var key=root.CreateSubKey(appExe)) key.SetValue("AppID",Clsid);
            using(var key=root.CreateSubKey(clsid)) { key.SetValue(null,"PulsarFab regain Pegasus FocusCube3"); key.SetValue("AppID",Clsid); }
            using(var key=root.CreateSubKey(clsid+@"\LocalServer32")) key.SetValue(null,command);
            using(var key=root.CreateSubKey(clsid+@"\ProgID")) key.SetValue(null,ProgId);
            using(var key=root.CreateSubKey(progid+@"\CLSID")) key.SetValue(null,Clsid);
            using(var key=root.CreateSubKey(chooser)) key.SetValue(null,"PulsarFab regain Pegasus FocusCube3");
        }
    }
}
