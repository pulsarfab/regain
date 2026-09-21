using Microsoft.Win32;

internal static class EtaRegistration
{
    internal const string ProgId = "ASCOM.Regain.ETA.Focuser";
    internal const string Clsid = "{C12BF695-204B-48B6-B6C6-0B90F238AB7F}";
    internal static void Register(RegistryKey root, string directory, bool remove)
    {
        string server = Path.Combine(directory,"Regain.Eta.ASCOM.exe");
        string command = "\"" + server + "\" /Embedding";
        string clsid = @"Software\Classes\CLSID\" + Clsid;
        string progid = @"Software\Classes\" + ProgId;
        string appid = @"Software\Classes\AppID\" + Clsid;
        string appExe = @"Software\Classes\AppID\Regain.Eta.ASCOM.exe";
        string chooser = @"Software\ASCOM\Focuser Drivers\" + ProgId;
        if(remove) {
            using var installed=root.OpenSubKey(clsid+@"\LocalServer32");
            if(installed is not null && !string.Equals(installed.GetValue(null) as string,command,StringComparison.OrdinalIgnoreCase))
                throw new InvalidOperationException("Unregister ETA from its currently installed directory.");
            if(installed is null) return;
            root.DeleteSubKeyTree(clsid,false); root.DeleteSubKeyTree(progid,false); root.DeleteSubKeyTree(chooser,false);
            root.DeleteSubKeyTree(appid,false); root.DeleteSubKeyTree(appExe,false);
        } else {
            if(!File.Exists(server)) throw new FileNotFoundException("ETA shared server is missing",server);
            // Match the ASCOM LocalServer template: a single interactive identity
            // also shares the server between elevated and ordinary clients.
            using(var key=root.CreateSubKey(appid)) { key.SetValue(null,"PulsarFab regain ETA shared server"); key.SetValue("RunAs","Interactive User"); }
            using(var key=root.CreateSubKey(appExe)) key.SetValue("AppID",Clsid);
            using(var key=root.CreateSubKey(clsid)) { key.SetValue(null,"PulsarFab regain Wanderer Astro ETA M54"); key.SetValue("AppID",Clsid); }
            using(var key=root.CreateSubKey(clsid+@"\LocalServer32")) key.SetValue(null,command);
            using(var key=root.CreateSubKey(clsid+@"\ProgID")) key.SetValue(null,ProgId);
            using(var key=root.CreateSubKey(progid+@"\CLSID")) key.SetValue(null,Clsid);
            using(var key=root.CreateSubKey(chooser)) key.SetValue(null,"PulsarFab regain Wanderer Astro ETA M54");
        }
    }
}
