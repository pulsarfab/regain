using System.Diagnostics;
using NINA.Core.Utility;
using NINA.Equipment.Interfaces.Mediator;

namespace Regain.NINA;

internal static class MountRecoveryStop
{
    internal static async Task RunAsync(RegainMount mount, IGuiderMediator guider,
        Func<IGreenSwampApi> greenSwampApi, Task sensorOperation)
    {
        mount.RecoveryFailed();
        Logger.Warning("Regain mount recovery failed: stopping guiding and mount motion, disabling tracking. No park or further recovery motion will be attempted.");
        void TrackingOff()
        {
            try { mount.TrackingEnabled = false; }
            catch (Exception error) { Logger.Error("Regain could not command tracking off: " + error); }
        }
        // A blocked guide stop or AbortSlew must not delay the first tracking-off
        // request. AbortSlew can itself restore tracking in some drivers, so
        // disable tracking again after the actual abort returns.
        var tracking = Task.Run(TrackingOff);
        var guide = Task.Run(async () => {
            try {
                if (guider.GetDevice() is { Connected: true } && !await guider.StopGuiding(CancellationToken.None).ConfigureAwait(false))
                    Logger.Error("Regain could not confirm guiding stopped after mount recovery failed.");
            } catch (Exception error) { Logger.Error("Regain guide stop failed: " + error); }
        });
        bool motionAccepted = true;
        var motion = Task.Run(async () => {
            if (mount.IsGreenSwamp) {
                try { await GreenSwampAutoHome.StopAsync(mount, greenSwampApi, sensorOperation).ConfigureAwait(false); }
                catch (Exception error) { motionAccepted = false; Logger.Error("Regain GSS emergency stop is unconfirmed; inspect mount: " + error); }
            } else {
                try { mount.StopSlew(); }
                catch (Exception error) { motionAccepted = false; Logger.Error("Regain mount abort failed: " + error); }
                finally { TrackingOff(); }
            }
        });
        await Task.WhenAll(tracking, guide, motion).ConfigureAwait(false);
        var confirmation = Stopwatch.StartNew();
        try {
            while (true) {
                if (!motionAccepted) break;
                if (mount.TrackingEnabled) TrackingOff();
                if (mount.Connected && !mount.TrackingEnabled && !mount.Slewing) {
                    Logger.Warning("Regain mount recovery aborted: tracking off and no slew reported; inspection required. No park commanded.");
                    return;
                }
                if (confirmation.Elapsed.TotalSeconds >= 5) break;
                await Task.Delay(250).ConfigureAwait(false);
            }
        } catch (Exception error) { Logger.Error("Regain cannot read mount stop confirmation: " + error); }
        Logger.Error("Regain mount stop/tracking-off is NOT confirmed. Recovery remains fenced; inspect equipment. No park will be attempted.");
    }
}
