# Recover an ASCOM mount in NINA

Development feature: source builds on `feature/ascom-mount-recovery` only.
It is not included in the published v0.6.1.5 packages. Physical mount acceptance
is still required before unattended use.

The **PulsarFab regain Mount — <driver>** choices wrap NINA's existing ASCOM
telescope adapters. Choose the entry for your installed ZWO or other mount
driver in NINA's Telescope equipment list. The gear button opens that driver's
own setup dialog while disconnected. This adds no native ASCOM Chooser entry,
direct serial driver, Alpaca telescope endpoint, or extra serial connection.

Normal telescope properties, coordinate epochs, slews, flips, pulse guiding and
site settings are delegated to NINA's adapter. The wrapper observes tracking
and celestial coordinates once per second after connection. Its Driver Info
shows monitoring, a latched fault, or recovery in progress.

## Sequence setup

Inside the active Deep Sky Object target, add **Recover Regain mount on tracking
loss** to its triggers. Enable the normal target initialization, plate solver,
and guiding settings first. Select the matching Regain mount provider.

The trigger checks before the next **LIGHT** exposure. The monitor detects
faults during an exposure, but this first implementation allows that exposure
to finish. It does not cancel or automatically remove the affected image.
Flats, darks, bias frames, and other instructions do not initiate recovery.
The separate **Recover Regain mount** action performs the same recovery when
explicitly reached in a target sequence.

| Shared option | Default | Meaning |
| --- | --- | --- |
| `LostTrackingSeconds` | 5 seconds | Required duration of unexpected tracking loss, connection failure, invalid position, or an uncommanded coordinate jump. |
| `CoordinateJumpDegrees` | 5 degrees | Angular jump from the last healthy celestial position. RA wrap and polar geometry use spherical separation. |
| `MotionTimeoutSeconds` | 180 seconds | Total recovery deadline, including homing, return slew, centering and guiding. Range 10–900 seconds. |

An explicit stop-tracking, park, or home command through NINA disarms recovery.
Commanded slews, flips and manual-axis movement suppress position-jump detection.
A fresh healthy tracking observation or an explicit tracking-enable command
arms monitoring again. Commands sent by other applications or a hand controller
cannot be distinguished from failures while the NINA recovery trigger is armed.

## Recovery order

1. Freeze the active sequence target, selected wrapper/profile, last healthy
   tracking rate, and whether guiding was active. Never use the mount's possibly
   reset RA/Dec as the return target.
2. Confirm the selected safety monitor is safe and available, when one is in use.
   Stop guiding before moving the mount.
3. Reconnect the same adapter if necessary, then home and wait for both `AtHome`
   and completion of slewing. A parked mount requires explicit inspection and
   unparking; the recovery never unparks it automatically.
4. Restore the saved tracking rate, enable tracking, and acknowledge the return
   slew. Use NINA's centering engine to plate-solve and center the saved target.
   Its configured solver and centering tolerances apply. The recovery adapter
   does not use the standard action's automatic guiding restart: guiding stays
   stopped until the solve and tracking checks succeed.
5. Verify tracking state/rate, restore previously active guiding using NINA's
   normal settling workflow, and verify the mount is no longer slewing.

If tracking was intentionally disabled, the action performs no movement and
does not enable tracking. Sidereal, lunar, solar, King and finite custom rates
use the same saved-rate path. Homing must be supported and trustworthy on the
specific mount; the wrapper does not infer physical position from reset telemetry.

## Failure and cancellation

Recovery has one attempt. Before it starts, the action/trigger persists
`ReconciliationRequired` in its sequence settings. Only complete success clears
that fence. Failure or cancellation interrupts the containing sequence, so a
failed trigger cannot silently fall through to the next exposure.

After inspecting equipment and the log, **Allow another recovery after inspecting
mount…** clears the fence. It cannot clear an operation that is still in flight.
NINA's instruction retry settings never replay an uncertain home or slew while
the fence remains set. The wrapper retains ownership of an uncooperative vendor
task after the caller's deadline; its late completion cannot start a return
slew or resume guiding. A cancelled physical move may still require intervention.

Safety, selected mount and active profile are checked between recovery phases.
Normal NINA weather shutdown and meridian-flip behavior remain responsible for
their own operations. Automatic recovery is restricted to light-exposure
boundaries and requires a valid active target.

The connected-but-not-tracking case is the initial acceptance target. If NINA
fully tears down or replaces its mount adapter, reconnect through NINA and
establish a new healthy tracking observation first. Recovery does not resurrect
a disposed adapter or assume that a newly selected mount is the previous one.

Logs go through NINA's normal logger. Search for `Regain mount fault` and
`Regain mount recovery` to see the observed failure and each recovery phase.

## Validation

Tests exercise tracking loss while connected, controller-like pole-coordinate
reset, intentional stops, RA wrap, stored target/rate preservation, complete
home/slew/center/guiding order, failed slews, unsafe transitions, cancellation
with a vendor operation still pending, deadline expiry during a blocked getter,
NINA sequence save/reload fencing, and centering without premature guiding.
These are deterministic fixtures, not AM7 hardware certification.
