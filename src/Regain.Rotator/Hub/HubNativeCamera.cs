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

    /// Reading/cancelling a finite image stream never retires control or sends
    /// equipment commands. A completed image belongs to exactly one acquisition.
    public async Task<HubCameraImage> DownloadCameraImageAsync(Guid expectedEpoch, HubImageBudget budget,
        CancellationToken cancellation = default)
    {
        HubClient current; HubAttachment attachment; HubCameraTiming timing;
        lock (gate) {
            if (expectedEpoch != epoch || client?.IsConnected != true || cameraTiming is null || cameraAttachment is null)
                throw new HubException(HubFailure.Disconnected);
            current = client; timing = cameraTiming; attachment = cameraAttachment;
        }
        // The ordinary client still bounds this scalar read. Caller cancellation
        // leaves its bounded read pending until its reply, without closing control.
        var status = await current.RequestAsync(JsonSerializer.SerializeToElement(new { op = "get", output = timing.Output,
            property = new { member = "cameraAcquisition" } }), cancellation).ConfigureAwait(false);
        cancellation.ThrowIfCancellationRequested();
        lock (gate) {
            if (expectedEpoch != epoch || !ReferenceEquals(current, client) || !current.IsConnected)
                throw new HubException(HubFailure.Disconnected);
        }
        var request = HubCameraProtocol.CompletedImage(current.Hello, timing.Output, status);
        if (request.Source != timing.Source) throw new HubException(HubFailure.Protocol);
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
