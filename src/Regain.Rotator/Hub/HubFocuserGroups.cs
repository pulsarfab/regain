using System.Globalization;
using System.Text.Json;

namespace Regain.Hub;

/// Explicit coordination over the same private host used by ordinary devices.
/// Status is retained host state; it never opens equipment or resumes a move.
public sealed class HubFocuserGroups : IDisposable
{
    private readonly Func<JsonElement, CancellationToken, Task<JsonElement>> request;
    private readonly Action close;
    private readonly JsonElement description, saved;
    private readonly SemaphoreSlim requests = new(1, 1);
    private readonly HashSet<Guid> uncertainStarts = [];
    private readonly Dictionary<Guid, JsonElement> observations = [];
    private int disposed;
    public Guid HostInstance { get; }
    public Guid Revision { get; }
    public IReadOnlyList<JsonElement> Groups { get; }
    public JsonElement TargetDescription => description.GetProperty("target");

    internal HubFocuserGroups(Guid host, JsonElement description, JsonElement saved,
        Func<JsonElement, CancellationToken, Task<JsonElement>> request, Action close)
    {
        if (host == Guid.Empty) throw new HubException(HubFailure.Protocol);
        HostInstance = host; Revision = HubWire.Identity(saved, "revision");
        this.description = description.Clone(); this.saved = saved.Clone(); this.request = request; this.close = close;
        if (description.GetProperty("configurationKey").GetString() != "focuserGroups" ||
            description.GetProperty("statusOpensSources").GetBoolean() || description.GetProperty("cancelHaltsEquipment").GetBoolean())
            throw new HubException(HubFailure.Protocol);
        Groups = Array.AsReadOnly(saved.TryGetProperty("focuserGroups", out var groups)
            ? groups.EnumerateArray().Select(g => g.Clone()).ToArray() : Array.Empty<JsonElement>());
        if (Groups.Count > 64) throw new HubException(HubFailure.Protocol);
    }

    public static async Task<HubFocuserGroups> AttachAsync(string executable, string configPath, Guid instance,
        CancellationToken cancellation = default)
    {
        var attachment = await HubAttachment.AttachAsync(executable, configPath, cancellation: cancellation, expectedInstance: instance).ConfigureAwait(false);
        var client = await HubClient.ConnectAsync(attachment, cancellation: cancellation).ConfigureAwait(false);
        try {
            client.RequireCapabilities("focuserGroups");
            var description = await client.RequestAsync(JsonSerializer.SerializeToElement(new { op = "describeConfig" }), cancellation).ConfigureAwait(false);
            var config = await client.RequestAsync(JsonSerializer.SerializeToElement(new { op = "getConfig" }), cancellation).ConfigureAwait(false);
            if (HubWire.Identity(config, "instanceId") != instance || HubWire.Identity(config, "revision") != client.Hello.ConfigurationRevision)
                throw new HubException(HubFailure.Protocol);
            return new(client.Hello.HostInstance, description.GetProperty("coordination").GetProperty("focuserGroups"), config, client.RequestAsync, client.Dispose);
        } catch { client.Dispose(); throw; }
    }

    public int ParseTarget(Guid group, string text)
    {
        var config = Group(group);
        if (!int.TryParse(text, NumberStyles.Integer, CultureInfo.InvariantCulture, out var target) ||
            target < config.GetProperty("minimum").GetInt32() || target > config.GetProperty("maximum").GetInt32())
            throw new InvalidOperationException("Enter a logical target within the saved group bounds");
        // The authoritative host validates transformed targets and live limits.
        return target;
    }
    public Task<JsonElement> StartAsync(Guid group, int target, CancellationToken cancellation = default)
    {
        ParseTarget(group, target.ToString(CultureInfo.InvariantCulture));
        return Call("startOperation", group, null, target, cancellation);
    }
    public Task<JsonElement> StatusAsync(Guid group, Guid? operation = null, CancellationToken cancellation = default)
        => Call("statusOperation", group, operation, null, cancellation);
    public Task<JsonElement> CancelAsync(Guid group, Guid operation, CancellationToken cancellation = default)
    {
        if (operation == Guid.Empty) throw new ArgumentException("Select a retained operation to cancel", nameof(operation));
        return Call("cancelOperation", group, operation, null, cancellation);
    }
    private JsonElement Group(Guid group)
    {
        if (Volatile.Read(ref disposed) != 0) throw new ObjectDisposedException(nameof(HubFocuserGroups));
        foreach (var config in Groups) if (config.GetProperty("id").GetGuid() == group) return config;
        throw new InvalidOperationException("Select a saved focuser group; apply and reload new definitions first");
    }
    private async Task<JsonElement> Call(string kind, Guid group, Guid? operation, int? target, CancellationToken cancellation)
    {
        var config = Group(group);
        if (operation == Guid.Empty) throw new ArgumentException("Invalid operation identity", nameof(operation));
        await requests.WaitAsync(cancellation).ConfigureAwait(false);
        var sent = false;
        try {
            Group(group);
            if (kind == "startOperation" && uncertainStarts.Contains(group))
                throw new InvalidOperationException("The previous start has an unknown outcome. Read retained status and explicitly reattach before another start.");
            using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
            timer.CancelAfter(TimeSpan.FromSeconds(10));
            object command = kind == "startOperation" ? new { op = description.GetProperty(kind).GetString(), group, target = target!.Value, expectedRevision = Revision } :
                new { op = description.GetProperty(kind).GetString(), group, operation, expectedRevision = Revision };
            sent = true;
            var result = await request(JsonSerializer.SerializeToElement(command), timer.Token).ConfigureAwait(false);
            Validate(result, config, operation, target);
            observations[group] = result.Clone(); return result.Clone();
        } catch (Exception error) {
            if (sent && kind == "startOperation" && !(error is HubException hub &&
                (hub.Failure is HubFailure.Busy or HubFailure.InvalidRequest || hub.Failure == HubFailure.Remote &&
                 hub.Remote?.Code is not ("uncertain" or "timeout" or "disconnected")))) uncertainStarts.Add(group);
            throw;
        } finally { requests.Release(); }
    }
    private void Validate(JsonElement result, JsonElement config, Guid? expectedOperation, int? expectedTarget)
    {
        try {
            HubDiagnosticContract.Schema(description.GetProperty("responseSchema"), result);
            void Require(bool condition) { if (!condition) throw new HubException(HubFailure.Protocol); }
            var group = config.GetProperty("id").GetGuid(); var operation = HubWire.Identity(result, "operation");
            var target = result.GetProperty("logicalTarget").GetInt32(); var sequence = result.GetProperty("sequence").GetUInt64();
            Require(result.GetProperty("hostInstance").GetGuid() == HostInstance && result.GetProperty("configurationRevision").GetGuid() == Revision &&
                result.GetProperty("group").GetGuid() == group && (!expectedOperation.HasValue || expectedOperation == operation) &&
                (!expectedTarget.HasValue || expectedTarget == target) && sequence > 0 &&
                target >= config.GetProperty("minimum").GetInt32() && target <= config.GetProperty("maximum").GetInt32());
            if (expectedTarget.HasValue) Require(result.GetProperty("phase").GetString() == "connecting" && sequence == 1 && result.GetProperty("result").ValueKind == JsonValueKind.Null);
            if (observations.TryGetValue(group, out var old) && old.GetProperty("operation").GetGuid() == operation) {
                Require(!expectedTarget.HasValue);
                if (Terminal(old)) Require(HubDiagnosticContract.Equal(old, result));
                Require(old.GetProperty("logicalTarget").GetInt32() == target && sequence >= old.GetProperty("sequence").GetUInt64());
                if (sequence == old.GetProperty("sequence").GetUInt64()) Require(HubDiagnosticContract.Equal(old, result));
                var oldReport = old.GetProperty("result"); var nextReport = result.GetProperty("result");
                if (oldReport.ValueKind != JsonValueKind.Null && nextReport.ValueKind != JsonValueKind.Null) {
                    Require(nextReport.GetProperty("sequence").GetUInt64() >= oldReport.GetProperty("sequence").GetUInt64());
                    var oldMembers = oldReport.GetProperty("members").EnumerateArray().ToArray(); var nextMembers = nextReport.GetProperty("members").EnumerateArray().ToArray();
                    Require(oldMembers.Length == nextMembers.Length);
                    for (var i = 0; i < oldMembers.Length; i++) Require(oldMembers[i].GetProperty("generation").GetGuid() == nextMembers[i].GetProperty("generation").GetGuid());
                }
            }
            var bindings = result.GetProperty("bindings").EnumerateArray().ToArray(); var members = config.GetProperty("members").EnumerateArray().ToArray();
            Require(bindings.Length == members.Length && bindings.Select(b => b.GetProperty("physicalSource").GetGuid()).Distinct().Count() == members.Length);
            for (var i = 0; i < members.Length; i++) Require(bindings[i].GetProperty("configuredSource").GetGuid() == members[i].GetProperty("source").GetGuid() &&
                bindings[i].GetProperty("physicalSource").GetGuid() == PhysicalSource(members[i].GetProperty("source").GetGuid()));
            var failed = result.GetProperty("failedSource");
            Require(failed.ValueKind == JsonValueKind.Null || bindings.Any(b => b.GetProperty("physicalSource").GetGuid() == failed.GetGuid()));
            var phase = result.GetProperty("phase").GetString(); var report = result.GetProperty("result");
            if (report.ValueKind == JsonValueKind.Null) { Require(phase is "connecting" or "failed" or "cancelled" or "deadline"); return; }
            Require(report.GetProperty("operation").GetGuid() == operation && report.GetProperty("group").GetGuid() == group &&
                report.GetProperty("logicalTarget").GetInt32() == target && report.GetProperty("sequence").GetUInt64() > 0);
            var innerPhase = report.GetProperty("phase").GetString();
            Require(phase == "failed" || phase == (innerPhase is "preflight" or "moving" ? "running" : innerPhase));
            var results = report.GetProperty("members").EnumerateArray().ToArray(); Require(results.Length == members.Length);
            for (var i = 0; i < results.Length; i++) {
                var member = results[i];
                Require(member.GetProperty("source").GetGuid() == bindings[i].GetProperty("physicalSource").GetGuid());
                var memberTarget = CalibratedTarget(members[i], target);
                Require(member.GetProperty("target").GetInt32() == memberTarget);
                if (member.GetProperty("phase").GetString() == "complete") Require(member.GetProperty("lastPosition").GetInt32() == memberTarget && member.GetProperty("error").ValueKind == JsonValueKind.Null);
            }
            if (phase == "complete") Require(results.All(m => m.GetProperty("phase").GetString() == "complete") && result.GetProperty("error").ValueKind == JsonValueKind.Null);
        } catch (HubException) { throw; }
        catch { throw new HubException(HubFailure.Protocol); }
    }
    private Guid PhysicalSource(Guid source)
    {
        var seen = new HashSet<Guid>();
        while (seen.Count < 256 && seen.Add(source)) {
            var backend = saved.GetProperty("sources").EnumerateArray().Single(s => s.GetProperty("id").GetGuid() == source).GetProperty("backend");
            if (backend.GetProperty("kind").GetString() != "virtual") return source;
            var output = backend.GetProperty("output").GetGuid();
            var device = saved.GetProperty("outputs").EnumerateArray().Single(o => o.GetProperty("id").GetGuid() == output).GetProperty("device");
            if (device.GetProperty("kind").GetString() != "proxy" || device.GetProperty("deviceType").GetString() != "focuser") break;
            source = device.GetProperty("source").GetGuid();
        }
        throw new HubException(HubFailure.Protocol);
    }
    private static int CalibratedTarget(JsonElement member, int target)
    {
        var numerator = (long)target * member.GetProperty("scaleNumerator").GetInt32();
        var denominator = member.GetProperty("scaleDenominator").GetInt32();
        var scaled = numerator / denominator;
        if (Math.Abs(numerator % denominator) * 2 >= denominator) scaled += Math.Sign(numerator);
        return checked((int)(scaled + member.GetProperty("offset").GetInt32()));
    }
    public static bool Terminal(JsonElement status) => status.GetProperty("phase").GetString() is not ("connecting" or "running");
    public static string Summary(JsonElement status)
    {
        var lines = new List<string> { "Group " + status.GetProperty("phase").GetString() + " · logical target " + status.GetProperty("logicalTarget") + " · operation " + status.GetProperty("operation").GetString() };
        if (status.GetProperty("failedSource").ValueKind != JsonValueKind.Null) lines.Add("Source " + status.GetProperty("failedSource").GetString() + " failed during connection or capability admission.");
        if (status.GetProperty("result").ValueKind != JsonValueKind.Null) foreach (var member in status.GetProperty("result").GetProperty("members").EnumerateArray())
            lines.Add(member.GetProperty("source").GetString() + ": " + member.GetProperty("phase").GetString() + " · target " + member.GetProperty("target") + " · last position " + member.GetProperty("lastPosition") +
                (member.GetProperty("error").ValueKind == JsonValueKind.Null ? "" : " · " + member.GetProperty("error").GetProperty("message").GetString()));
        if (status.GetProperty("error").ValueKind != JsonValueKind.Null) lines.Add(status.GetProperty("error").GetProperty("message").GetString()!);
        return string.Join("\n", lines);
    }
    public void Dispose() { if (Interlocked.Exchange(ref disposed, 1) == 0) close(); }
}
