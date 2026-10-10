using System.Text.Json;

namespace Regain.Hub;

public sealed partial class HubNativeSession
{
    private HubAttachment? cameraAttachment;
    private HubCameraTiming? cameraTiming;

    public async Task<JsonElement> RequestCameraAsync(Guid expectedEpoch, JsonElement command, CancellationToken cancellation = default)
    {
        HubClient current; HubCameraTiming timing;
        lock (gate) {
            if (expectedEpoch != epoch || client?.IsConnected != true || cameraTiming is null)
                throw new HubException(HubFailure.Disconnected);
            current = client; timing = cameraTiming;
        }
        try {
            var value = await current.RequestCameraAsync(timing, command, cancellation).ConfigureAwait(false);
            lock (gate) {
                if (expectedEpoch != epoch || !ReferenceEquals(current, client) || !current.IsConnected)
                    throw new HubException(HubFailure.Disconnected);
            }
            return value;
        } catch (HubException error) when (error.Failure is HubFailure.Remote or HubFailure.Busy or HubFailure.InvalidRequest) { throw; }
        catch { Retire(expectedEpoch); throw; }
    }

    /// Polling readiness is observational. Caller cancellation leaves the
    /// bounded scalar reply pending; it never aborts or retires camera control.
    public async Task<JsonElement> ReadCameraAcquisitionAsync(Guid expectedEpoch, CancellationToken cancellation = default)
    {
        HubClient current; HubCameraTiming timing;
        lock (gate) {
            if (expectedEpoch != epoch || client?.IsConnected != true || cameraTiming is null)
                throw new HubException(HubFailure.Disconnected);
            current = client; timing = cameraTiming;
        }
        var status = await current.RequestAsync(JsonSerializer.SerializeToElement(new { op = "get", output = timing.Output,
            property = new { member = "cameraAcquisition" } }), cancellation).ConfigureAwait(false);
        cancellation.ThrowIfCancellationRequested();
        lock (gate) {
            if (expectedEpoch != epoch || !ReferenceEquals(current, client) || !current.IsConnected)
                throw new HubException(HubFailure.Disconnected);
        }
        if (HubWire.Identity(status, "source") != timing.Source) throw new HubException(HubFailure.Protocol);
        return status;
    }

    /// Reading/cancelling a finite image stream never retires control or sends
    /// equipment commands. A completed image belongs to exactly one acquisition.
    public Task<HubCameraImage> DownloadCameraImageAsync(Guid expectedEpoch, HubImageBudget budget,
        CancellationToken cancellation = default)
        => DownloadCameraImageCoreAsync(expectedEpoch, budget, null, cancellation);

    /// A frontend which initiated a capture must request that exact acquisition,
    /// rather than accidentally returning a sibling's later frame.
    public Task<HubCameraImage> DownloadCameraImageAsync(Guid expectedEpoch, HubImageBudget budget,
        Guid acquisition, CancellationToken cancellation = default)
    {
        if (acquisition == Guid.Empty) throw new HubException(HubFailure.InvalidRequest);
        return DownloadCameraImageCoreAsync(expectedEpoch, budget, acquisition, cancellation);
    }

    private async Task<HubCameraImage> DownloadCameraImageCoreAsync(Guid expectedEpoch, HubImageBudget budget,
        Guid? acquisition, CancellationToken cancellation)
    {
        HubClient current; HubAttachment attachment; HubCameraTiming timing;
        lock (gate) {
            if (expectedEpoch != epoch || client?.IsConnected != true || cameraTiming is null || cameraAttachment is null)
                throw new HubException(HubFailure.Disconnected);
            current = client; timing = cameraTiming; attachment = cameraAttachment;
        }
        var status = await ReadCameraAcquisitionAsync(expectedEpoch, cancellation).ConfigureAwait(false);
        lock (gate) {
            if (expectedEpoch != epoch || !ReferenceEquals(current, client) || !current.IsConnected)
                throw new HubException(HubFailure.Disconnected);
        }
        var request = HubCameraProtocol.CompletedImage(current.Hello, timing.Output, status);
        if (request.Source != timing.Source) throw new HubException(HubFailure.Protocol);
        if (acquisition.HasValue && request.Acquisition != acquisition.Value)
            throw new HubException(HubFailure.Remote, new HubRemoteError(JsonSerializer.SerializeToElement(new {
                code = "unavailable", message = "The requested camera acquisition has been replaced" })));
        var image = await HubCameraImages.DownloadAsync(attachment, current, request, budget,
            HubCameraImages.MaximumTransferTime, cancellation).ConfigureAwait(false);
        try {
            cancellation.ThrowIfCancellationRequested();
            lock (gate) {
                if (expectedEpoch != epoch || !ReferenceEquals(current, client) || !current.IsConnected)
                    throw new HubException(HubFailure.Disconnected);
            }
            return image;
        } catch { image.Dispose(); throw; }
    }
}
