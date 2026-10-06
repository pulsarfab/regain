using Microsoft.Win32;
using Regain.Hub.ASCOM;

internal static class HubRegistration
{
    internal static int Run(string[] args, string directory)
    {
        using var mutex = new Mutex(false, @"Global\PulsarFab.Regain.HubRegistration");
        var held = false;
        try {
            try { held = mutex.WaitOne(TimeSpan.FromSeconds(5)); } catch (AbandonedMutexException) { held = true; }
            if (!held) throw new InvalidOperationException("Another process is changing hub registration");
            using var x86 = RegistryKey.OpenBaseKey(RegistryHive.LocalMachine, RegistryView.Registry32);
            using var x64 = RegistryKey.OpenBaseKey(RegistryHive.LocalMachine, RegistryView.Registry64);
            var roots = new[] { x86, x64 };
            if (args.Length == 6 && args[0].Equals("/hubregister", StringComparison.OrdinalIgnoreCase)) {
                HubAscomRegistration.RegisterSaved(roots, directory, args[1], Guid.ParseExact(args[2], "D"),
                    Guid.ParseExact(args[3], "D"), Guid.ParseExact(args[4], "D"), args[5]);
            } else if (args.Length == 3 && args[0].Equals("/hubunregister", StringComparison.OrdinalIgnoreCase)) {
                HubAscomRegistration.Remove(roots, directory, Guid.ParseExact(args[1], "D"), args[2]);
            } else if (args.Length == 1 && args[0].Equals("/hubunregisterall", StringComparison.OrdinalIgnoreCase)) {
                var ids = roots.SelectMany(root => HubAscomRegistration.RegisteredIds(root, directory)).Distinct().ToArray();
                foreach (var id in ids) HubAscomRegistration.Remove(roots, directory, id, null);
            } else throw new ArgumentException("Unsupported hub registration command");
            return 0;
        } finally { if (held) mutex.ReleaseMutex(); }
    }
}
