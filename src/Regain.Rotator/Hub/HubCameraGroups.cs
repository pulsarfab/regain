using System.Globalization;
using System.Text.Json;

namespace Regain.Hub;

public sealed class HubCameraMemberRequest
{
    public Guid Source { get; }
    public double DurationSeconds { get; }
    public bool Light { get; }
    public HubCameraMemberRequest(Guid source, double durationSeconds, bool light)
    {
        if (source == Guid.Empty) throw new ArgumentException("Select a saved camera member", nameof(source));
        HubCameraProtocol.Start(durationSeconds, light);
        Source = source; DurationSeconds = durationSeconds; Light = light;
    }
    internal object Wire() => new { source = Source, exposure = new { durationSeconds = DurationSeconds, light = Light } };
}

/// Retained camera operations use the same private host and immutable image reader
/// as ordinary cameras. Reading status or images never starts equipment work.
public sealed class HubCameraGroups : IDisposable
{
    private readonly Func<JsonElement, CancellationToken, Task<JsonElement>> request;
    private readonly Func<HubGroupImageRequest, HubImageBudget, TimeSpan, CancellationToken, Task<HubCameraImage>>? download;
    private readonly Action close;
    private readonly JsonElement description, saved;
    private readonly SemaphoreSlim requests = new(1, 1);
    private readonly HashSet<Guid> uncertainStarts = [];
    private readonly Dictionary<Guid, JsonElement> observations = [];
    private int disposed;
    public Guid HostInstance { get; }
    public Guid Revision { get; }
    public IReadOnlyList<JsonElement> Groups { get; }

    internal HubCameraGroups(Guid host, JsonElement description, JsonElement saved,
        Func<JsonElement, CancellationToken, Task<JsonElement>> request, Action close,
        Func<HubGroupImageRequest, HubImageBudget, TimeSpan, CancellationToken, Task<HubCameraImage>>? download = null)
    {
        if (host == Guid.Empty) throw new HubException(HubFailure.Protocol);
        HostInstance = host; Revision = HubWire.Identity(saved, "revision");
        this.description = description.Clone(); this.saved = saved.Clone(); this.request = request; this.close = close; this.download = download;
        if (description.GetProperty("configurationKey").GetString() != "cameraGroups" ||
            description.GetProperty("statusOpensSources").GetBoolean() || description.GetProperty("imageOpensSources").GetBoolean() ||
            description.GetProperty("cancellationPolicy").GetString() != "savedGroupPolicy" ||
            description.GetProperty("imageOperation").GetString() != "cameraGroupImage") throw new HubException(HubFailure.Protocol);
        Groups = Array.AsReadOnly(saved.TryGetProperty("cameraGroups", out var groups)
            ? groups.EnumerateArray().Select(g => g.Clone()).ToArray() : Array.Empty<JsonElement>());
        if (Groups.Count > 64) throw new HubException(HubFailure.Protocol);
    }

    public static async Task<HubCameraGroups> AttachAsync(string executable, string configPath, Guid instance,
        CancellationToken cancellation = default)
    {
        var attachment = await HubAttachment.AttachAsync(executable, configPath, cancellation: cancellation, expectedInstance: instance).ConfigureAwait(false);
        var client = await HubClient.ConnectAsync(attachment, cancellation: cancellation).ConfigureAwait(false);
        try {
            client.RequireCapabilities("cameraGroups", "cameraAcquisition", "cameraImageStream");
            var description = await client.RequestAsync(JsonSerializer.SerializeToElement(new { op = "describeConfig" }), cancellation).ConfigureAwait(false);
            var config = await client.RequestAsync(JsonSerializer.SerializeToElement(new { op = "getConfig" }), cancellation).ConfigureAwait(false);
            if (HubWire.Identity(config, "instanceId") != instance || HubWire.Identity(config, "revision") != client.Hello.ConfigurationRevision)
                throw new HubException(HubFailure.Protocol);
            return new(client.Hello.HostInstance, description.GetProperty("coordination").GetProperty("cameraGroups"), config, client.RequestAsync, client.Dispose,
                (identity, budget, deadline, token) => HubCameraImages.DownloadAsync(attachment, client, identity, budget, deadline, token));
        } catch { client.Dispose(); throw; }
    }

    public IReadOnlyList<HubCameraMemberRequest> UniformRequests(Guid group, double durationSeconds, bool light)
        => Array.AsReadOnly(Group(group).GetProperty("members").EnumerateArray().Select(m => new HubCameraMemberRequest(m.GetGuid(), durationSeconds, light)).ToArray());
    public Task<JsonElement> StartAsync(Guid group, IReadOnlyList<HubCameraMemberRequest> members, CancellationToken cancellation = default)
    {
        var config = Group(group); var frozen = members.ToArray();
        if (frozen.Length != config.GetProperty("members").GetArrayLength() || frozen.Where((m, i) => m is null || m.Source != config.GetProperty("members")[i].GetGuid()).Any())
            throw new ArgumentException("Requests must match the saved camera members in order", nameof(members));
        return Call("startOperation", group, null, JsonSerializer.SerializeToElement(frozen.Select(m => m.Wire())), cancellation);
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
        if (Volatile.Read(ref disposed) != 0) throw new ObjectDisposedException(nameof(HubCameraGroups));
        foreach (var config in Groups) if (config.GetProperty("id").GetGuid() == group) return config;
        throw new InvalidOperationException("Select a saved camera group; apply and reload new definitions first");
    }
    private async Task<JsonElement> Call(string kind, Guid group, Guid? operation, JsonElement? exposures, CancellationToken cancellation)
    {
        var config = Group(group);
        if (operation == Guid.Empty) throw new ArgumentException("Invalid operation identity", nameof(operation));
        await requests.WaitAsync(cancellation).ConfigureAwait(false);
        var sent = false;
        try {
            Group(group);
            if (kind == "startOperation" && uncertainStarts.Contains(group))
                throw new InvalidOperationException("The previous start has an unknown outcome. Read retained status and explicitly reattach before another start.");
            using var timer = CancellationTokenSource.CreateLinkedTokenSource(cancellation); timer.CancelAfter(TimeSpan.FromSeconds(10));
            object command = kind == "startOperation" ? new { op = description.GetProperty(kind).GetString(), group, requests = exposures!.Value, expectedRevision = Revision } :
                new { op = description.GetProperty(kind).GetString(), group, operation, expectedRevision = Revision };
            sent = true;
            var result = await request(JsonSerializer.SerializeToElement(command), timer.Token).ConfigureAwait(false);
            Group(group); Validate(result, config, operation, exposures);
            observations[group] = result.Clone(); return result.Clone();
        } catch (Exception error) {
            if (sent && kind == "startOperation" && HubGroupContract.UnknownStart(error)) uncertainStarts.Add(group);
            throw;
        } finally { requests.Release(); }
    }
    private void Validate(JsonElement status, JsonElement config, Guid? expectedOperation, JsonElement? expectedRequests)
    {
        try {
            HubDiagnosticContract.Schema(description.GetProperty("responseSchema"), status);
            void Require(bool condition) { if (!condition) throw new HubException(HubFailure.Protocol); }
            var group = config.GetProperty("id").GetGuid(); var operation = HubWire.Identity(status, "operation");
            var sequence = status.GetProperty("sequence").GetUInt64(); var phase = status.GetProperty("phase").GetString();
            Require(status.GetProperty("hostInstance").GetGuid() == HostInstance && status.GetProperty("configurationRevision").GetGuid() == Revision &&
                status.GetProperty("group").GetGuid() == group && (!expectedOperation.HasValue || expectedOperation == operation) && sequence > 0);
            var demands = status.GetProperty("requests"); var bindings = status.GetProperty("bindings").EnumerateArray().ToArray();
            var members = config.GetProperty("members").EnumerateArray().ToArray();
            Require(bindings.Length == members.Length && demands.GetArrayLength() == members.Length &&
                bindings.Select(b => b.GetProperty("physicalSource").GetGuid()).Distinct().Count() == members.Length);
            for (var i = 0; i < members.Length; i++) {
                Require(demands[i].GetProperty("source").GetGuid() == members[i].GetGuid() &&
                    demands[i].GetProperty("exposure").GetProperty("durationSeconds").GetDouble() >= 0 &&
                    bindings[i].GetProperty("configuredSource").GetGuid() == members[i].GetGuid() &&
                    bindings[i].GetProperty("physicalSource").GetGuid() == HubGroupContract.PhysicalSource(saved, members[i].GetGuid(), "camera"));
            }
            var report = status.GetProperty("result");
            if (expectedRequests.HasValue) Require(phase == "connecting" && sequence == 1 && report.ValueKind == JsonValueKind.Null && HubDiagnosticContract.Equal(demands, expectedRequests.Value));
            if (observations.TryGetValue(group, out var old) && old.GetProperty("operation").GetGuid() == operation) {
                Require(!expectedRequests.HasValue && sequence >= old.GetProperty("sequence").GetUInt64() && HubDiagnosticContract.Equal(old.GetProperty("requests"), demands));
                HubGroupContract.ImmutableTerminal(old, status);
                if (sequence == old.GetProperty("sequence").GetUInt64()) Require(HubDiagnosticContract.Equal(old, status));
                var previous = old.GetProperty("result");
                if (previous.ValueKind != JsonValueKind.Null) {
                    Require(report.ValueKind != JsonValueKind.Null && report.GetProperty("sequence").GetUInt64() >= previous.GetProperty("sequence").GetUInt64());
                    if (report.GetProperty("sequence").GetUInt64() == previous.GetProperty("sequence").GetUInt64()) Require(HubDiagnosticContract.Equal(previous, report));
                    for (var i = 0; i < members.Length; i++) {
                        var before = previous.GetProperty("members")[i]; var after = report.GetProperty("members")[i];
                        Require(before.GetProperty("generation").GetGuid() == after.GetProperty("generation").GetGuid());
                        if (before.GetProperty("acquisition").ValueKind != JsonValueKind.Null) Require(HubDiagnosticContract.Equal(before.GetProperty("acquisition"), after.GetProperty("acquisition")));
                        if (before.GetProperty("image").ValueKind != JsonValueKind.Null) Require(HubDiagnosticContract.Equal(before.GetProperty("image"), after.GetProperty("image")));
                    }
                }
            }
            var failed = status.GetProperty("failedSource");
            Require(failed.ValueKind == JsonValueKind.Null || bindings.Any(b => b.GetProperty("physicalSource").GetGuid() == failed.GetGuid()));
            if (report.ValueKind == JsonValueKind.Null) { Require(phase is "connecting" or "failed" or "cancelled" or "deadline"); return; }
            Require(report.GetProperty("operation").GetGuid() == operation && report.GetProperty("group").GetGuid() == group && report.GetProperty("sequence").GetUInt64() > 0);
            var innerPhase = report.GetProperty("phase").GetString();
            Require(phase == "failed" || phase == (innerPhase is "preflight" or "capturing" ? "running" : innerPhase));
            var results = report.GetProperty("members").EnumerateArray().ToArray(); Require(results.Length == members.Length);
            var dispatches = new List<double>();
            for (var i = 0; i < results.Length; i++) {
                var member = results[i]; var image = member.GetProperty("image"); var completed = member.GetProperty("phase").GetString() == "complete";
                Require(member.GetProperty("source").GetGuid() == bindings[i].GetProperty("physicalSource").GetGuid() &&
                    HubDiagnosticContract.Equal(member.GetProperty("request"), demands[i].GetProperty("exposure")) && completed == (image.ValueKind != JsonValueKind.Null));
                var dispatch = member.GetProperty("dispatchSeconds"); var acknowledgement = member.GetProperty("acknowledgementSeconds");
                Require((dispatch.ValueKind == JsonValueKind.Null) == (acknowledgement.ValueKind == JsonValueKind.Null));
                if (dispatch.ValueKind != JsonValueKind.Null) { Require(dispatch.GetDouble() >= 0 && acknowledgement.GetDouble() >= dispatch.GetDouble()); dispatches.Add(dispatch.GetDouble()); }
                if (completed) {
                    Require(member.GetProperty("error").ValueKind == JsonValueKind.Null && member.GetProperty("acquisition").ValueKind != JsonValueKind.Null &&
                        image.GetProperty("source").GetGuid() == member.GetProperty("source").GetGuid() && image.GetProperty("generation").GetGuid() == member.GetProperty("generation").GetGuid() &&
                        image.GetProperty("acquisition").GetGuid() == member.GetProperty("acquisition").GetGuid() && HubDiagnosticContract.Equal(image.GetProperty("request"), member.GetProperty("request")));
                    var geometry = image.GetProperty("geometry");
                    Require(geometry.GetProperty("width").GetUInt32() > 0 && geometry.GetProperty("height").GetUInt32() > 0 && geometry.GetProperty("binX").GetUInt32() > 0 && geometry.GetProperty("binY").GetUInt32() > 0);
                }
            }
            var skew = report.GetProperty("startSkewSeconds");
            if (skew.ValueKind != JsonValueKind.Null) Require(dispatches.Count == results.Length && Math.Abs(skew.GetDouble() - (dispatches.Max() - dispatches.Min())) <= 1e-9 && skew.GetDouble() >= 0);
            if (phase == "complete") Require(results.All(m => m.GetProperty("phase").GetString() == "complete") && status.GetProperty("error").ValueKind == JsonValueKind.Null);
        } catch (HubException) { throw; }
        catch { throw new HubException(HubFailure.Protocol); }
    }

    /// The source is the configured member ID, including aliases. Its exact
    /// physical image identity comes only from this client's validated status.
    public async Task<HubCameraImage> DownloadAsync(Guid group, Guid operation, Guid configuredSource, HubImageBudget budget,
        TimeSpan deadline, CancellationToken cancellation = default)
    {
        HubGroupImageRequest identity; JsonElement geometry;
        await requests.WaitAsync(cancellation).ConfigureAwait(false);
        try {
            var config = Group(group);
            if (download is null) throw new InvalidOperationException("This client has no protected image attachment");
            if (!observations.TryGetValue(group, out var status) || status.GetProperty("operation").GetGuid() != operation)
                throw new InvalidOperationException("Read this exact retained operation before downloading its images");
            var index = Array.FindIndex(config.GetProperty("members").EnumerateArray().ToArray(), m => m.GetGuid() == configuredSource);
            if (index < 0) throw new ArgumentException("Select a saved camera member", nameof(configuredSource));
            var member = status.GetProperty("result").ValueKind == JsonValueKind.Null ? default : status.GetProperty("result").GetProperty("members")[index];
            if (member.ValueKind == JsonValueKind.Undefined || member.GetProperty("image").ValueKind == JsonValueKind.Null)
                throw new InvalidOperationException("The selected member has no retained completed image");
            var image = member.GetProperty("image"); geometry = image.GetProperty("geometry").Clone();
            identity = new(HostInstance, Revision, group, operation, image.GetProperty("source").GetGuid(), image.GetProperty("generation").GetGuid(), image.GetProperty("acquisition").GetGuid());
        } finally { requests.Release(); }
        var result = await download(identity, budget, deadline, cancellation).ConfigureAwait(false);
        try {
            identity.Match(JsonSerializer.SerializeToElement(result.Request.Wire()));
            if (result.Descriptor.Width != geometry.GetProperty("width").GetUInt32() || result.Descriptor.Height != geometry.GetProperty("height").GetUInt32()) throw new HubException(HubFailure.Protocol);
            return result;
        } catch { result.Dispose(); throw; }
    }
    public static bool Terminal(JsonElement status) => HubGroupContract.Terminal(status);
    public static string Summary(JsonElement status)
    {
        var lines = new List<string> { "Group " + status.GetProperty("phase").GetString() + " · operation " + status.GetProperty("operation").GetString() };
        if (status.GetProperty("failedSource").ValueKind != JsonValueKind.Null) lines.Add("Source " + status.GetProperty("failedSource").GetString() + " failed during connection or capability admission.");
        if (status.GetProperty("result").ValueKind != JsonValueKind.Null) {
            var report = status.GetProperty("result"); var skew = report.GetProperty("startSkewSeconds");
            lines.Add(skew.ValueKind == JsonValueKind.Null ? "Host request spread is not yet known." : "Host request spread " + skew.GetDouble().ToString("G6", CultureInfo.InvariantCulture) + " s; sensor synchronization is not guaranteed.");
            foreach (var member in report.GetProperty("members").EnumerateArray())
                lines.Add(member.GetProperty("source").GetString() + ": " + member.GetProperty("phase").GetString() + " · exposure " + member.GetProperty("request").GetProperty("durationSeconds") + " s" +
                    (member.GetProperty("image").ValueKind == JsonValueKind.Null ? "" : " · retained image " + member.GetProperty("acquisition").GetString()) +
                    (member.GetProperty("error").ValueKind == JsonValueKind.Null ? "" : " · " + member.GetProperty("error").GetProperty("message").GetString()) +
                    (member.GetProperty("abortError").ValueKind == JsonValueKind.Null ? "" : " · abort: " + member.GetProperty("abortError").GetProperty("message").GetString()));
        }
        if (status.GetProperty("error").ValueKind != JsonValueKind.Null) lines.Add(status.GetProperty("error").GetProperty("message").GetString()!);
        return string.Join("\n", lines);
    }
    public void Dispose() { if (Interlocked.Exchange(ref disposed, 1) == 0) close(); }
}
