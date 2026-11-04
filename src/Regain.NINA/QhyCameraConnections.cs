using Regain.Core;

namespace Regain.NINA;

/// Camera and its 4-pin wheel share one supervisor and one physical handle.
internal static class QhyCameraConnections
{
    private static readonly SemaphoreSlim Gate = new(1);
    private static CameraSession? owner;
    private static CameraSelection? selection;
    private static int cameras, wheels;
    internal static bool InUse => owner is not null;
    internal static Task UpdateOptionsAsync(RecoveryOptions options) => owner?.UpdateRuntimeOptionsAsync(options) ?? Task.CompletedTask;
    internal sealed class Lease(CameraSession session, bool camera) : IDisposable
    {
        internal CameraSession Session { get; } = session;
        private int released;
        public void Dispose() {
            if (Interlocked.Exchange(ref released, 1) != 0) return;
            ReleaseAsync(camera).GetAwaiter().GetResult();
        }
    }
    internal static async Task<Lease> AcquireAsync(CameraSelection requested, bool camera, CancellationToken token)
    {
        await Gate.WaitAsync(token).ConfigureAwait(false);
        try {
            if (!requested.Camera.Name.StartsWith("QHY", StringComparison.Ordinal)) throw new InvalidOperationException("Select the parent QHY camera in Regain camera setup first");
            if (owner is not null) {
                if (selection!.Camera.Name != requested.Camera.Name || selection.UseDirectDriver != requested.UseDirectDriver || selection.AllowSdkFallback != requested.AllowSdkFallback ||
                    (requested.Serial is not null && requested.Serial != owner.Serial))
                    throw new InvalidOperationException("Disconnect the Regain camera and QHY wheel before changing their shared camera selection");
                if (camera && cameras != 0) throw new InvalidOperationException("This Regain camera already has an imaging client");
            } else {
                var candidate = new CameraSession(requested.Camera, requested.UseDirectDriver ? CameraProvider.NewDirectHost : CameraProvider.NewHost,
                    Settings.Load(), requested.Serial, requested.UseDirectDriver && requested.AllowSdkFallback ? CameraProvider.NewHost : null);
                // A wheel may open the shared owner first. Reserve the recovery
                // barrier now; the imaging lease supplies its current callback.
                candidate.RecoverUsbPeripherals = (_,_) => Task.CompletedTask;
                candidate.Diagnostic += text => CameraLog.Session(requested.Camera.Name, text);
                try { await candidate.ConnectAsync(token).ConfigureAwait(false); }
                catch { candidate.Dispose(); throw; }
                owner = candidate; selection = requested;
                Settings.Cameras.RememberSerial(requested, owner.Serial);
            }
            if (camera) cameras++; else wheels++;
            return new(owner, camera);
        } finally { Gate.Release(); }
    }
    private static async Task ReleaseAsync(bool camera)
    {
        await Gate.WaitAsync().ConfigureAwait(false);
        try {
            if (camera) cameras--; else wheels--;
            if (cameras + wheels == 0) { var old = owner; owner = null; selection = null; old?.Dispose(); }
            else if (camera && cameras == 0) await owner!.DisableThermalOutputsAsync().ConfigureAwait(false);
        } finally { Gate.Release(); }
    }
}
