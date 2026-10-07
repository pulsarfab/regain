using System.Text.Json;

namespace Regain.Hub;

/// Inert, revision-bound server deadlines. These do not change recovery policy.
public sealed class HubCameraTiming
{
    internal const long MaximumMilliseconds = int.MaxValue - 120000L;
    public Guid HostInstance { get; }
    public Guid ConfigurationRevision { get; }
    public Guid ClientId { get; }
    public Guid Output { get; }
    public Guid Source { get; }
    public bool Native { get; }
    public TimeSpan Connect { get; }
    public TimeSpan Start { get; }
    public TimeSpan Setting { get; }
    public TimeSpan Stop { get; }
    public TimeSpan Abort { get; }
    internal HubCameraTiming(JsonElement value, HubHello hello, Guid output)
    {
        HubWire.Members(value, "hostInstance", "configurationRevision", "clientId", "output", "source", "native",
            "connectMilliseconds", "startMilliseconds", "settingMilliseconds", "stopMilliseconds", "abortMilliseconds");
        HostInstance = HubWire.Identity(value, "hostInstance"); ConfigurationRevision = HubWire.Identity(value, "configurationRevision");
        ClientId = HubWire.Identity(value, "clientId"); Output = HubWire.Identity(value, "output"); Source = HubWire.Identity(value, "source");
        Native = value.GetProperty("native").GetBoolean();
        Connect = Bound(value, "connectMilliseconds"); Start = Bound(value, "startMilliseconds");
        Setting = Bound(value, "settingMilliseconds"); Stop = Bound(value, "stopMilliseconds"); Abort = Bound(value, "abortMilliseconds");
        if (!Matches(hello) || Output != output) throw new HubException(HubFailure.Protocol);
    }
    internal static TimeSpan Bound(JsonElement value, string key)
    {
        var milliseconds = value.GetProperty(key).GetInt64();
        if (milliseconds <= 0 || milliseconds > MaximumMilliseconds) throw new HubException(HubFailure.Protocol);
        return TimeSpan.FromMilliseconds(milliseconds);
    }
    internal bool Matches(HubHello hello) => HostInstance == hello.HostInstance &&
        ConfigurationRevision == hello.ConfigurationRevision && ClientId == hello.ClientId;
    internal TimeSpan DeadlineFor(JsonElement command)
    {
        try {
            if (command.GetProperty("output").GetGuid() != Output) throw new HubException(HubFailure.InvalidRequest);
            return command.GetProperty("op").GetString() switch {
                "connect" => Connect,
                "changeConnection" when command.GetProperty("connected").GetBoolean() && !command.GetProperty("asynchronous").GetBoolean() => Connect,
                "put" => command.GetProperty("property").GetProperty("member").GetString() switch {
                    "startExposure" => Start, "cameraSetting" or "pulseGuide" => Setting, "stopExposure" => Stop, "abortExposure" => Abort,
                    _ => throw new HubException(HubFailure.InvalidRequest)
                },
                _ => throw new HubException(HubFailure.InvalidRequest)
            };
        } catch (Exception) { throw new HubException(HubFailure.InvalidRequest); }
    }
}

public sealed partial class HubClient
{
    public async Task<HubCameraTiming> GetCameraTimingAsync(Guid output, CancellationToken cancellation = default)
    {
        if (output == Guid.Empty) throw new HubException(HubFailure.InvalidRequest);
        RequireCapabilities("cameraOperationTiming");
        var value = await RequestAsync(JsonSerializer.SerializeToElement(new { op = "cameraTiming", output,
            expectedRevision = Hello.ConfigurationRevision }), cancellation).ConfigureAwait(false);
        try { return new HubCameraTiming(value, Hello, output); }
        catch (HubException) { throw; }
        catch (Exception) { throw new HubException(HubFailure.Protocol); }
    }
    public Task<JsonElement> RequestCameraAsync(HubCameraTiming timing, JsonElement command, CancellationToken cancellation = default)
    {
        RequireCapabilities("cameraOperationTiming");
        if (!timing.Matches(Hello)) throw new HubException(HubFailure.InvalidRequest);
        var deadline = timing.DeadlineFor(command);
        return state.Request(JsonSerializer.SerializeToElement(new { op = "cameraControl",
            expectedRevision = timing.ConfigurationRevision, command }), cancellation, deadline);
    }
}
