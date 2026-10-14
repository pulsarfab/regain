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
An explicit tracking-enable command arms monitoring again after an intentional
stop. A healthy tracking observation also arms monitoring on initial connection.
Commands sent by other applications or a hand controller
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
   Tracking writes are issued once; verification allows up to five seconds for
   NINA's ASCOM telemetry cache to refresh, within the total recovery deadline.
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

Before moving, recovery also creates a durable per-mount marker under
`%LOCALAPPDATA%\PulsarFab\Regain\MountRecovery`. It survives a NINA restart or
loading an older sequence. The marker remains locked until the actual vendor
operation ends, including after a timeout. Complete recovery removes it;
inspection can clear it only with the original mount selected and no operation
still in flight. An unavailable/unwritable journal prevents recovery motion.

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

Review regression tests also use NINA's real running sequence container to
verify interruption cannot await its own action, allow cached tracking telemetry
to catch up without replaying writes, stop on invalid trigger settings, and
preserve the inspection fence across selection changes and journal reloads.

### Installed NINA simulator acceptance (2026-10-13)

NINA 3.2.0.9001 loaded the development plugin with a separate simulator-only
profile: Regain wrapped `ASCOM.Simulator.Telescope` from the installed ASCOM
Omni-Simulators, and NINA Simulator Camera replayed a 6248 × 4176 RAW16 star-field
FITS. The local Seiza executable in ASTAP-compatible mode performed the solve.
The sequence fixture is
[`mount-recovery-sequence.json`](../tests/fixtures/nina/mount-recovery-sequence.json).

| Case | Observed result |
| --- | --- |
| Tracking disabled externally during a 30-second light | Fault latched after the debounce; the active capture finished before homing began. |
| Successful recovery | Home completed, J2000 target was transformed through NINA's adapter, return slew completed, real plate solve/centering succeeded, and the next two-second light was acquired and saved. Journal removed. |
| Injected local solver failure | Home/return slew completed; centering failed; sequence stopped without a subsequent camera acquisition. Journal retained. |
| Original sequence reloaded after failure | Despite `ReconciliationRequired=false` in the file, the journal blocked the next light without another home, slew, or camera acquisition. |
| Inspection confirmed in NINA | The inspection control changed from on to off and removed the completed simulator attempt's journal. |

The pass exposed and fixed stale ASCOM tracking telemetry after writes. Full
Core and NINA suites pass (178 and 625 tests respectively; five hardware/COM
opt-ins skipped). Guiding stop/restart/settling is covered by deterministic
mediator fixtures; no live guider was available in this installed-NINA pass.
Physical AM7 homing, guiding and observatory safety acceptance remain required.
