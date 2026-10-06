using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using Regain.Hub;

// This executable runs the same public attachment/client API in real net48
// x86/x64 processes. Its caller creates a unique simulated configuration.
internal static class Program
{
    private static async Task<int> Main(string[] args)
    {
        uint? candidate = null;
        try {
            if (args.Length != 3 || IntPtr.Size * 8 != int.Parse(args[2])) throw new InvalidOperationException("Wrong fixture bitness");
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(40));
            var attachment = await HubAttachment.AttachAsync(args[0], args[1], cancellation: deadline.Token);
            candidate = attachment.StartedProcessId;
            if (candidate is null) throw new InvalidOperationException("Fixture expected to launch its unique test host");
            using var first = await HubClient.ConnectAsync(attachment, cancellation: deadline.Token);
            var attached = await HubAttachment.AttachAsync(args[0], args[1], cancellation: deadline.Token);
            if (attached.StartedProcessId is not null || attached.HostInstance != attachment.HostInstance)
                throw new InvalidOperationException("Shared host identity changed");
            using var second = await HubClient.ConnectAsync(attached, cancellation: deadline.Token);
            if (first.Hello.ClientId == second.Hello.ClientId) throw new InvalidOperationException("Clients were not independent");
            var saved = await first.RequestAsync(JsonSerializer.SerializeToElement(new { op = "getConfig" }), deadline.Token);
            var output = saved.GetProperty("outputs")[0].GetProperty("id").GetGuid();
            var connect = JsonSerializer.SerializeToElement(new { op = "changeConnection", output, connected = true, asynchronous = false });
            await first.RequestAsync(connect, deadline.Token); await second.RequestAsync(connect, deadline.Token);
            var connected = JsonSerializer.SerializeToElement(new { op = "get", output, property = new { member = "connected" } });
            first.Dispose();
            if (!(await second.RequestAsync(connected, deadline.Token)).GetBoolean()) throw new InvalidOperationException("One client revoked another");
            await second.RequestAsync(JsonSerializer.SerializeToElement(new { op = "changeConnection", output, connected = false, asynchronous = false }), deadline.Token);
            if ((await second.RequestAsync(connected, deadline.Token)).GetBoolean()) throw new InvalidOperationException("Disconnect retained this lease");
            second.Dispose();
            using var probe = await HubClient.ConnectAsync(attached, cancellation: deadline.Token);
            if (probe.Hello.HostInstance != attachment.HostInstance) throw new InvalidOperationException("Frontend disconnect stopped the host");
            Console.WriteLine($"net48 {IntPtr.Size * 8}-bit: shared identity, separate leases, disconnect and surviving host passed");
            return 0;
        } catch (Exception error) { Console.Error.WriteLine(error.GetType().Name + ": " + error.Message); return 1; }
        finally {
            // Test-only cleanup, never used by production frontends. The script
            // supplies a fresh simulation-only config and launches sequentially.
            if (candidate is uint pid) {
                try {
                    using var host = Process.GetProcessById(checked((int)pid));
                    var path = new StringBuilder(32768); uint length = (uint)path.Capacity;
                    if (!QueryFullProcessImageName(host.Handle, 0, path, ref length) ||
                        !string.Equals(System.IO.Path.GetFullPath(args[0]), path.ToString(), StringComparison.OrdinalIgnoreCase))
                        throw new InvalidOperationException("Fixture host executable did not match; refusing cleanup");
                    host.Kill();
                    if (!host.WaitForExit(5000)) throw new InvalidOperationException("Fixture cleanup timed out");
                } catch (ArgumentException) { }
            }
        }
    }
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool QueryFullProcessImageName(IntPtr process, uint flags, StringBuilder path, ref uint length);
}
