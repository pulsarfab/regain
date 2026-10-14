# Recover an ASCOM mount in NINA

Available from v0.6.1.6 as an opt-in NINA feature. Physical mount acceptance is
still required before unattended use.

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
| `GreenSwampAutoHome` | On | For `ASCOM.GS.Sky.Telescope`, use GSS sensor AutoHome instead of its coordinate-only ASCOM FindHome. Other drivers are unaffected. |
| `GreenSwampSearchDegrees` | 100 degrees | Maximum GSS sensor search, 20–179 degrees. A failed search is never extended or retried automatically. |
| `GreenSwampDeclinationOffsetDegrees` | 0 degrees | GSS's declination offset for the sensor home, −90–90 degrees; configure for your mounting arrangement. |

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

## Green Swamp Server sensor AutoHome

The GSS sensor path is under development after v0.6.1.6. The basic wrapper in
that release uses standard ASCOM FindHome for every driver.

For the registered **ASCOM.GS.Sky.Telescope** driver, sensor AutoHome is enabled
by default in the recovery action and trigger. Regain connects to the installed
**GS.SkyApi** COM interface in the same GSS server. GSS keeps ownership of the
serial port. Driver identity selects this path; a similar display name does not.

GSS must be connected to a mount with home sensors. Regain stops guiding and
tracking, confirms both axes stopped, and temporarily disables GSS's processing
of ASCOM movement commands. It calls `AutoHomeStart(searchDegrees, decOffset)`
once, observes `IsAutoHomeRunning` start and finish, checks `LastAutoHomeError`,
and requires both axes stopped, `AtHome` and no slew before restoring ASCOM
movement and continuing the normal target/centering/guiding recovery.

No sensors, an unavailable API, a rejected start, an incomplete search, uncertain
home state or a failed API call stops recovery. There is no automatic fallback to
GSS's ordinary FindHome: that operation only slews to GSS's stored home coordinates
and cannot reconstruct lost axis position. You can explicitly turn off
`GreenSwampAutoHome` to use ordinary FindHome when coordinate homing is appropriate.

Cancel from NINA to cancel the entire recovery. Regain calls `AutoHomeStop()` and
keeps the recovery operation owned until GSS reports homing ended and both axes
stopped. ASCOM movement remains disabled during that wait. A lost connection or
blocked COM call can outlive the sequence deadline; the durable inspection fence
prevents another attempt. The helper never closes GSS or opens its serial port.

GSS 1.2.2.7's `LastAutoHomeError` getter dereferences a null exception after a
successful start. **Unreadable status is a recovery failure**, including this
null-reference bug (`E_POINTER` over COM). Regain aborts, disables tracking and
retains the inspection fence; it never infers success from home coordinates.
With that GSS version, sensor homing can run, but automatic target return is
blocked until GSS provides readable completion/error status. GSS also leaves the
error field empty for a stop requested in its own UI and exposes no independent
per-axis success result. Use NINA cancellation to stop the entire recovery.

Configure the search and offset for your mount and cable travel. Sensor search
can move a mount whose original position is unknown, and plate solving happens
only after homing. Physical GSS sensor-homing acceptance remains required. GSS's
simulator does not pass this API's SkyWatcher-only `CanHomeSensors` getter, so a
GSS simulator run cannot certify this sensor path.

Protocol references: [GSS scripting API](https://greenswamp.org/?docs=gs-server-overview%2Fscripting-api),
[Home versus AutoHome](https://greenswamp.org/?docs=gs-server-overview%2Fmain-tab%2Fbutton-control),
and inspected upstream commit
[`03df875`](https://github.com/rmorgan001/GSServer/tree/03df8752eab71856b84ca9f5a5febc28e8de6ed6):
[`GS.SkyApi/Sky.cs`](https://github.com/rmorgan001/GSServer/blob/03df8752eab71856b84ca9f5a5febc28e8de6ed6/GS.SkyApi/Sky.cs)
and [`SkyServer.AutoHomeAsync`](https://github.com/rmorgan001/GSServer/blob/03df8752eab71856b84ca9f5a5febc28e8de6ed6/GS.Server/SkyTelescope/SkyServer.cs).
Regain uses the public COM interface without bundling GSS code or binaries.

## Failure and cancellation

Recovery has one attempt. Before it starts, the action/trigger persists
`ReconciliationRequired` in its sequence settings. Only complete success clears
that fence. Failure or cancellation interrupts the containing sequence, so a
failed trigger cannot silently fall through to the next exposure.

**Every runtime recovery failure, cancellation or deadline aborts mount motion
and disables tracking. Regain never parks, unparks, retries homing, returns to
the target or restarts guiding after failure.** The original mount is stopped
even if NINA's selected mount or profile changes. Guiding is stopped as well.
Tracking-off is sent independently of the abort, then repeated after AbortSlew
returns because some drivers restore tracking during abort. If a recovery command
finishes late, the stop/tracking-off sequence runs again afterward before ownership
is released. Failed or fenced action execution also requests a stop instead of
allowing imaging to continue.

The failed wrapper also rejects further slews, home, park/unpark, manual-axis
movement and tracking enable until **Allow another recovery after inspecting
mount…** clears the failure. Its normal telemetry poll commands tracking off
again if the vendor server re-enables it later. It does not launch another home,
park or abort from that poll. Clearing inspection leaves tracking off; it does
not resume imaging or guiding automatically.

For GSS, cleanup requests `AutoHomeStop` and both axis stops through a separate
connection to the same server. After the homing owner's gate cleanup finishes,
Regain temporarily enables ASCOM command processing, aborts and sets tracking off
last; this avoids GSS silently ignoring tracking writes while its gate is closed.
The previous command-gate state is restored. No serial connection is opened.

The sequence caller waits at most six seconds for cleanup before returning the
original failure. Actual stop commands keep ownership and the journal lock until
they finish. NINA logs distinguish **tracking off/no slew reported** from **stop
or tracking-off NOT confirmed**. A disconnected or unresponsive driver cannot
prove the hardware stopped; recovery stays fenced for inspection and never tries
a park as a fallback.

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

GSS fixtures cover the full stop-guiding/sensor-home/return/center/guiding path,
late-bound API members, settings editing and NINA sequence serialization,
unsupported/busy/disconnected/parked servers, failed and unacknowledged starts,
missing home confirmation, safety changes and cancellation with homing still
running. They verify no coordinate-home fallback and retained ownership until
the real server operation stops. These are API fixtures, not physical sensor tests.
The development sensor-home/failure-stop change passes the full Core and NINA
suites (185 and 655 tests; five explicit hardware/COM skips). Its WPF settings test runs
outside parallel UI collections to avoid WPF schema/type-initialization deadlock.
Failure fixtures cover homing, return slew, centering, guiding and safety faults,
AbortSlew re-enabling tracking or throwing, late tracking writes, blocked aborts
retaining the durable journal lock, invalid/fenced action execution, persistent
tracking-off enforcement and blocked shutdown parking, and zero park or
unpark commands throughout the mount recovery suite.

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
