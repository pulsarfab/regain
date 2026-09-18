using System.Diagnostics;
using Microsoft.Win32;
using ZwoGain.Ascom;

internal static class Program
{
    [STAThread]
    static int Main(string[] args)
    {
        string logDir = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "ZwoGain", "ASCOM");
        try {
            if (args.Length == 2 && args[0].Equals("/checkinuse", StringComparison.OrdinalIgnoreCase)) {
                var running = InUseCheck.Find(args[1]);
                if (running.Length == 0) return 0;
                Directory.CreateDirectory(logDir);
                File.AppendAllText(Path.Combine(logDir, "registration.log"), "Close before setup: " + string.Join(", ", running) + Environment.NewLine);
                return 2;
            }
            bool remove = args.Any(a => a.Equals("/unregserver", StringComparison.OrdinalIgnoreCase));
            if (args.Any(a => a.Equals("/rotator", StringComparison.OrdinalIgnoreCase))) {
                using var rotator = new CaaRotator(); rotator.SetupDialog(); return 0;
            }
            if (!remove && !args.Any(a => a.Equals("/regserver", StringComparison.OrdinalIgnoreCase))) {
                using var camera = new Camera1(); camera.SetupDialog(); return 0;
            }
            string assembly = typeof(Camera1).Assembly.Location;
            foreach (var view in new[] { RegistryView.Registry32, RegistryView.Registry64 }) {
                using var root = RegistryKey.OpenBaseKey(RegistryHive.LocalMachine, view);
                // Uninstall only this copy; an older directory must not remove a newer install.
                if (remove) for (int slot = 1; slot <= 4; slot++) {
                    using var existing = root.OpenSubKey(@"Software\Classes\CLSID\{D1DB6F94-5CC0-4752-A758-F849098874A" + slot + @"}\InprocServer32");
                    if (existing is not null && (!Uri.TryCreate(existing.GetValue("CodeBase") as string, UriKind.Absolute, out var installed) ||
                        !installed.IsFile || !string.Equals(Path.GetFullPath(installed.LocalPath), assembly, StringComparison.OrdinalIgnoreCase)))
                        throw new InvalidOperationException("Unregister from the currently installed ZWOgain directory.");
                }
                if (remove) {
                    using var existing = root.OpenSubKey(@"Software\Classes\CLSID\{A918164B-49DD-4FF5-BEE6-A4AB93B97F12}\InprocServer32");
                    if (existing is not null && (!Uri.TryCreate(existing.GetValue("CodeBase") as string, UriKind.Absolute, out var installed) ||
                        !installed.IsFile || !string.Equals(Path.GetFullPath(installed.LocalPath), assembly, StringComparison.OrdinalIgnoreCase)))
                        throw new InvalidOperationException("Unregister the rotator from its currently installed directory.");
                }
                string framework = view == RegistryView.Registry32 ? "Framework" : "Framework64";
                string regasm = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Windows), "Microsoft.NET", framework, "v4.0.30319", "RegAsm.exe");
                using var child = Process.Start(new ProcessStartInfo(regasm, "\"" + assembly + "\" /nologo " + (remove ? "/unregister" : "/codebase")) {
                    UseShellExecute = false, CreateNoWindow = true, RedirectStandardOutput = true, RedirectStandardError = true
                })!;
                var output = child.StandardOutput.ReadToEndAsync(); var errors = child.StandardError.ReadToEndAsync();
                child.WaitForExit();
                Directory.CreateDirectory(logDir);
                File.AppendAllText(Path.Combine(logDir, "registration.log"), output.GetAwaiter().GetResult() + errors.GetAwaiter().GetResult());
                if (child.ExitCode != 0) throw new InvalidOperationException("COM registration failed. Run as administrator; see registration.log.");
                for (int slot = 1; slot <= 4; slot++) {
                    string key = @"Software\ASCOM\Camera Drivers\ASCOM.ZWOgain.Camera" + slot;
                    if (remove) root.DeleteSubKeyTree(key, false);
                    else { using var chooser = root.CreateSubKey(key); chooser.SetValue(null, "ZWOgain Retryable Camera " + slot); }
                }
                string rotatorKey = @"Software\ASCOM\Rotator Drivers\ASCOM.ZWOgain.Rotator";
                if (remove) root.DeleteSubKeyTree(rotatorKey, false);
                else { using var chooser = root.CreateSubKey(rotatorKey); chooser.SetValue(null, "ZWOgain CAA Rotator"); }
            }
            return 0;
        } catch (Exception error) {
            try { Directory.CreateDirectory(logDir); File.AppendAllText(Path.Combine(logDir, "registration.log"), error + Environment.NewLine); } catch { }
            return 1;
        }
    }
}
