using System.Text.Json;
using System.Text.Json.Nodes;
using Regain.Hub;

internal static class CameraGroupFixture
{
    internal static async Task Run(string executable, string path)
    {
        uint? owned = null;
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(20));
        try {
            var attachment = await HubAttachment.AttachAsync(executable, path, cancellation: deadline.Token);
            owned = attachment.StartedProcessId;
            if (owned is null) throw new InvalidOperationException("Camera group fixture must own its fresh private host");
            using var first = await HubCameraGroups.AttachAsync(executable, path, attachment.InstanceId, deadline.Token);
            var group = first.Groups.Single().GetProperty("id").GetGuid(); var requests = first.UniformRequests(group, 0.2, true);
            var started = await first.StartAsync(group, requests, deadline.Token); first.Dispose();
            using var second = await HubCameraGroups.AttachAsync(executable, path, attachment.InstanceId, deadline.Token);
            var operation = started.GetProperty("operation").GetGuid(); JsonElement status;
            do {
                status = await second.StatusAsync(group, operation, deadline.Token);
                if (!HubCameraGroups.Terminal(status)) await Task.Delay(20, deadline.Token);
            } while (!HubCameraGroups.Terminal(status));
            if (status.GetProperty("phase").GetString() != "complete") throw new InvalidOperationException("Camera group lost retained completion");
            var budget = new HubImageBudget(1024 * 1024);
            using (var a = await second.DownloadAsync(group, operation, requests[0].Source, budget, TimeSpan.FromSeconds(10), deadline.Token))
            using (var b = await second.DownloadAsync(group, operation, requests[1].Source, budget, TimeSpan.FromSeconds(10), deadline.Token)) {
                if (a.Request is not HubGroupImageRequest identity || identity.Operation != operation || identity.Group != group ||
                    a.Request.Acquisition == b.Request.Acquisition || a.Request.Source == b.Request.Source ||
                    budget.UsedBytes != a.ByteLength + b.ByteLength) throw new InvalidOperationException("Camera group changed image identity or budget");
                using var pin = a.Pin(); a.Dispose();
                var bytes = new byte[256]; pin.CopyTo(0, bytes, 0, bytes.Length);
                using var repeated = await second.DownloadAsync(group, operation, requests[0].Source, new HubImageBudget(1024 * 1024), TimeSpan.FromSeconds(10), deadline.Token);
                var copy = new byte[bytes.Length]; repeated.CopyTo(0, copy, 0, copy.Length);
                if (!bytes.SequenceEqual(copy) || repeated.Request.Acquisition != pin.Request.Acquisition) throw new InvalidOperationException("Retained image reread changed pixels or acquisition");
            }
            if (budget.UsedBytes != 0) throw new InvalidOperationException("Camera group image pins leaked budget");
            using var inspector = await HubClient.ConnectAsync(attachment, cancellation: deadline.Token);
            using var saved = JsonDocument.Parse(File.ReadAllText(path));
            foreach (var source in saved.RootElement.GetProperty("sources").EnumerateArray()) {
                JsonElement health;
                do {
                    health = await inspector.RequestAsync(JsonSerializer.SerializeToElement(new { op = "sourceStatus", source = source.GetProperty("id").GetGuid() }), deadline.Token);
                    if (health.GetProperty("leaseCount").GetInt32() != 0) await Task.Delay(10, deadline.Token);
                } while (health.GetProperty("leaseCount").GetInt32() != 0);
            }
            var description = await inspector.RequestAsync(JsonSerializer.SerializeToElement(new { op = "describeConfig" }), deadline.Token);
            foreach (var inner in new[] { false, true }) {
                var retained = JsonNode.Parse(status.GetRawText())!.AsObject(); var target = inner ? retained["result"]! : retained;
                target["sequence"] = 9007199254740992UL;
                using var guarded = new HubCameraGroups(second.HostInstance, description.GetProperty("coordination").GetProperty("cameraGroups"), saved.RootElement,
                    (_, _) => Task.FromResult(JsonSerializer.SerializeToElement(retained)), () => { });
                await guarded.StatusAsync(group, operation, deadline.Token); target["sequence"] = 9007199254740993UL;
                try { await guarded.StatusAsync(group, operation, deadline.Token); throw new InvalidOperationException("Rounded terminal sequence was accepted"); }
                catch (HubException error) when (error.Failure == HubFailure.Protocol) { }
            }
            Console.WriteLine($"net48 {IntPtr.Size * 8}-bit: camera group, alias mapping, retained IPC reattachment, separate exact images, rereads, shared budget and zero output leases passed");
        } finally { if (owned is uint pid) Program.StopOwned(pid, executable); }
    }
}
