using System.Text.Json;
using Regain.Hub;

internal static class FocuserGroupFixture
{
    internal static async Task Run(string executable, string path)
    {
        uint? owned = null;
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(20));
        try {
            var attachment = await HubAttachment.AttachAsync(executable, path, cancellation: deadline.Token);
            owned = attachment.StartedProcessId;
            if (owned is null) throw new InvalidOperationException("Group fixture must own its fresh private host");
            using var first = await HubFocuserGroups.AttachAsync(executable, path, attachment.InstanceId, deadline.Token);
            var group = first.Groups.Single().GetProperty("id").GetGuid();
            var started = await first.StartAsync(group, 50050, deadline.Token); first.Dispose();
            using var second = await HubFocuserGroups.AttachAsync(executable, path, attachment.InstanceId, deadline.Token);
            JsonElement status;
            do {
                status = await second.StatusAsync(group, started.GetProperty("operation").GetGuid(), deadline.Token);
                if (!HubFocuserGroups.Terminal(status)) await Task.Delay(20, deadline.Token);
            } while (!HubFocuserGroups.Terminal(status));
            if (status.GetProperty("phase").GetString() != "complete" ||
                !status.GetProperty("result").GetProperty("members").EnumerateArray().Select(m => m.GetProperty("lastPosition").GetInt32()).SequenceEqual(new[] { 50050, 50250 }))
                throw new InvalidOperationException("Group lost completion or calibrated member results");
            using var inspector = await HubClient.ConnectAsync(attachment, cancellation: deadline.Token);
            using var saved = JsonDocument.Parse(File.ReadAllText(path));
            foreach (var source in saved.RootElement.GetProperty("sources").EnumerateArray()) {
                JsonElement health;
                do {
                    health = await inspector.RequestAsync(JsonSerializer.SerializeToElement(new { op = "sourceStatus", source = source.GetProperty("id").GetGuid() }), deadline.Token);
                    if (health.GetProperty("leaseCount").GetInt32() != 0) await Task.Delay(10, deadline.Token);
                } while (health.GetProperty("leaseCount").GetInt32() != 0);
            }
            Console.WriteLine($"net48 {IntPtr.Size * 8}-bit: calibrated group, alias mapping, retained IPC reattachment, exact member results and lease cleanup passed");
        } finally { if (owned is uint pid) Program.StopOwned(pid, executable); }
    }
}
