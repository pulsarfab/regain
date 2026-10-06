using System;
using System.IO;
using System.Threading;

// A private stand-in for a shared vendor helper. Stop through its own marker,
// never by terminating an arbitrary shared server discovered on the machine.
internal static class Program {
    private static int Main(string[] args) {
        if (args.Length != 1) return 2;
        var limit = DateTime.UtcNow.AddMinutes(3);
        while (!File.Exists(args[0]) && DateTime.UtcNow < limit) Thread.Sleep(20);
        return 0;
    }
}
