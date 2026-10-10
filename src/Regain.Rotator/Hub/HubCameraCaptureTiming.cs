using System.Text.Json;

namespace Regain.Hub;

/// Duration-dependent host bounds after StartExposure acknowledgement. Reading
/// these neither acquires equipment nor grants native recovery to a proxy.
public sealed class HubCameraCaptureTiming
{
    public Guid HostInstance { get; }
    public Guid ConfigurationRevision { get; }
    public Guid ClientId { get; }
    public Guid Output { get; }
    public Guid Source { get; }
    public bool Native { get; }
    public double DurationSeconds { get; }
    public TimeSpan Readiness { get; }
    public TimeSpan Completion { get; }
    internal HubCameraCaptureTiming(JsonElement value, HubHello hello, Guid output, double seconds)
    {
        HubWire.Members(value, "hostInstance", "configurationRevision", "clientId", "output", "source", "native",
            "durationSeconds", "readinessMilliseconds", "completionMilliseconds");
        HostInstance = HubWire.Identity(value, "hostInstance"); ConfigurationRevision = HubWire.Identity(value, "configurationRevision");
        ClientId = HubWire.Identity(value, "clientId"); Output = HubWire.Identity(value, "output"); Source = HubWire.Identity(value, "source");
        Native = value.GetProperty("native").GetBoolean(); DurationSeconds = value.GetProperty("durationSeconds").GetDouble();
        Readiness = HubCameraTiming.Bound(value, "readinessMilliseconds"); Completion = HubCameraTiming.Bound(value, "completionMilliseconds");
        if (HostInstance != hello.HostInstance || ConfigurationRevision != hello.ConfigurationRevision || ClientId != hello.ClientId ||
            Output != output || double.IsNaN(DurationSeconds) || double.IsInfinity(DurationSeconds) || DurationSeconds < 0 ||
            DurationSeconds != seconds || Readiness.TotalSeconds < DurationSeconds || Completion < Readiness)
            throw new HubException(HubFailure.Protocol);
    }
}

public sealed partial class HubClient
{
    public async Task<HubCameraCaptureTiming> GetCameraCaptureTimingAsync(Guid output, double seconds, CancellationToken cancellation = default)
    {
        if (output == Guid.Empty || double.IsNaN(seconds) || double.IsInfinity(seconds) || seconds < 0)
            throw new HubException(HubFailure.InvalidRequest);
        RequireCapabilities("cameraCaptureTiming");
        var value = await RequestAsync(JsonSerializer.SerializeToElement(new { op = "cameraCaptureTiming", output,
            expectedRevision = Hello.ConfigurationRevision, durationSeconds = seconds }), cancellation).ConfigureAwait(false);
        try { return new HubCameraCaptureTiming(value, Hello, output, seconds); }
        catch (HubException) { throw; }
        catch (Exception) { throw new HubException(HubFailure.Protocol); }
    }
}

public sealed partial class HubNativeSession
{
    public async Task<HubCameraCaptureTiming> GetCameraCaptureTimingAsync(Guid expectedEpoch, double seconds, CancellationToken cancellation = default)
    {
        HubClient current; HubCameraTiming timing;
        lock (gate) {
            if (expectedEpoch != epoch || client?.IsConnected != true || cameraTiming is null)
                throw new HubException(HubFailure.Disconnected);
            current = client; timing = cameraTiming;
        }
        var value = await current.GetCameraCaptureTimingAsync(timing.Output, seconds, cancellation).ConfigureAwait(false);
        lock (gate) {
            if (expectedEpoch != epoch || !ReferenceEquals(current, client) || !current.IsConnected)
                throw new HubException(HubFailure.Disconnected);
        }
        if (value.Source != timing.Source || value.Native != timing.Native) throw new HubException(HubFailure.Protocol);
        return value;
    }
}
