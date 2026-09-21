using Microsoft.Win32;

internal static class Ofp2Registration
{
    internal const string ProgId = "ASCOM.Regain.OFP2.CoverCalibrator";
    internal const string Clsid = "{8E24512B-6BC6-4A44-9488-53E63C68CCB7}";
    internal static void Register(RegistryKey root, string directory, bool remove)
    {
        string server = Path.Combine(directory,"Regain.Ofp2.ASCOM.exe");
        string command = "\"" + server + "\" /Embedding";
        string clsid = @"Software\Classes\CLSID\" + Clsid;
        string progid = @"Software\Classes\" + ProgId;
        string appid = @"Software\Classes\AppID\" + Clsid;
        string appExe = @"Software\Classes\AppID\Regain.Ofp2.ASCOM.exe";
        string chooser = @"Software\ASCOM\CoverCalibrator Drivers\" + ProgId;
        if(remove) {
            using var installed=root.OpenSubKey(clsid+@"\LocalServer32");
            if(installed is not null && !string.Equals(installed.GetValue(null) as string,command,StringComparison.OrdinalIgnoreCase))
                throw new InvalidOperationException("Unregister OFP2 from its currently installed directory.");
            if(installed is null) return;
            root.DeleteSubKeyTree(clsid,false); root.DeleteSubKeyTree(progid,false); root.DeleteSubKeyTree(chooser,false);
            root.DeleteSubKeyTree(appid,false); root.DeleteSubKeyTree(appExe,false);
        } else {
            if(!File.Exists(server)) throw new FileNotFoundException("OFP2 shared server is missing",server);
            // Match the ASCOM LocalServer template: a single interactive identity
            // also shares the server between elevated and ordinary clients.
            using(var key=root.CreateSubKey(appid)) { key.SetValue(null,"PulsarFab regain OFP2 shared server"); key.SetValue("RunAs","Interactive User"); }
            using(var key=root.CreateSubKey(appExe)) key.SetValue("AppID",Clsid);
            using(var key=root.CreateSubKey(clsid)) { key.SetValue(null,"PulsarFab regain Deep Sky Dad OFP2"); key.SetValue("AppID",Clsid); }
            using(var key=root.CreateSubKey(clsid+@"\LocalServer32")) key.SetValue(null,command);
            using(var key=root.CreateSubKey(clsid+@"\ProgID")) key.SetValue(null,ProgId);
            using(var key=root.CreateSubKey(progid+@"\CLSID")) key.SetValue(null,Clsid);
            using(var key=root.CreateSubKey(chooser)) key.SetValue(null,"PulsarFab regain Deep Sky Dad OFP2");
        }
    }
}
