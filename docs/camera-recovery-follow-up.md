# Camera control and recovery follow up

The October 11–12 Starfront run completed eight hours of science imaging with
Regain 0.6.0.7. Its logs showed two USB control outages, five failed NINA capture
requests during the longer outage, and a log watcher running out of memory at
the first 1 GiB log rollover. These changes address the observed delays and
repeated failures. They do not establish the cause of the USB outages.

## Completion checklist

Each item needs code or a documented scope decision, regression evidence where
failure behavior changes, and a final status before this work is considered done.

| Improvement | Required behavior | Validation | Status |
| --- | --- | --- | --- |
| Unchanged cooler commands | Refresh identical encoded outputs after five seconds since acknowledgement; changed outputs remain immediate within the slew limit. Keep temperature feedback and PI updates. Force output restoration after initialization/reconnect and explicit shutdown. Do not cache failed writes. | FPGA/DAC clock-policy, failed acknowledgement and heater-preset regressions; full-frame periodic keepalive, changed targets and shutdown passed. | Complete |
| Bounded failure diagnostics | Optional register snapshots cannot delay required clock restoration with multiple full USB deadlines. Preserve terminal I/O drain and uncertain-stop behavior. | ASI2600/6200 unavailable-transport fixtures and complete physical cancellation matrix passed. | Complete |
| Explicit transfer deadlines | Distinguish deadline expiry, late completion and OS error codes. A late successful completion after cancellation cannot appear as unexplained `failed: 0`. | Windows late-completion/wait-failure and shared flight-recorder regressions passed. Native transport has no equivalent Windows completion race and is unchanged. | Complete |
| Coordinated connection recovery | Telemetry and capture share one recovery state, identity and backoff. Preserve a living supervisor after worker faults; do not probe identity for background telemetry. | Actual simulated child-process faults, repeated polling, finite cached telemetry and bounded recovery tests passed. | Complete |
| Pre-exposure recovery | Retry a missing connection before arming independently of the permission to replace a long exposure. Keep all frontend timeout budgets consistent. No extra arm after an uncertain start. | 600-second request fixtures with replacement disabled, forbidden-write replay, generated configuration and timing checks passed. | Complete |
| Optional USB escalation | Evaluate reset eligibility during pre-exposure outages without enabling resets by default or treating them as exposure replacements. | Disabled-default, threshold, one-reset, identity/budget/permission and long-exposure fixtures passed. No physical reset performed. | Complete; opt-in |
| Reduced control traffic | Compare unchanged cooler writes with SDK behavior. Preserve required feedback, a slower unchanged-output keepalive, and camera-specific restoration. | Updated full-frame direct/SDK matrices passed; unchanged direct output refreshed every 5.079–5.285 seconds. | Complete |
| Circular diagnostics | Keep the existing default, bounded files, failure batches and live mode changes. Confirm first transport faults trigger the recorder, including deadline races. | Recorder/IPC/NINA tests and live circular/verbose/off changes during full-frame capture passed. | Complete |
| External log watcher | The failing `BackgroundWorker.OnLogFileChanged` is outside Regain. Keep the issue separate; incremental bounded reading belongs in the owning plugin. | Regain's bounded circular logging remains the default; the external watcher was not changed. | External follow up |

## Physical acceptance

Use the attached ASI6200MM Pro, pinned by serial, with full-frame RAW16 bin 1
captures. Record build hashes, SDK/USB command timing, frame dimensions and
completeness, temperature/power, sensor initialization and actuator writes.
Include repeated capture, stable regulation, changed targets, cooler/heater
on/off, live configuration, explicit close, and the full cancellation matrix.
Use deterministic fixtures to exercise faults the healthy camera does not
produce. Healthy hardware acceptance is not proof that the remote USB outage
has been reproduced or eliminated.

## Validation results

The baseline direct trace sent 24 cooler commands, including 21 consecutive
repeats. The comparable SDK trace sent seven commands, mostly changing its
output, with active updates roughly 5.59 seconds apart. This establishes
unnecessary direct traffic, not a hardware timeout contract. The revised policy
retains a conservative five-second unchanged-output keepalive while avoiding
the old one-second repeats; changed outputs remain immediate within the slew
limit. Acknowledgement time, rather than attempted dispatch time, starts the
keepalive interval. Heater presets are still written only when changed or
explicitly restored; this cooler policy does not add a heater keepalive.

The outage command-spacing audit used producer microsecond timestamps rather
than asynchronous NINA log delivery times. At 22:51:49 in the supplied log, the
last cooler write had completed **888.231 milliseconds** before the failing BC00
read. At 03:32:42, the failing BD26 cooler write began **27 microseconds** after
the B3 temperature read completed; preceding successful pairs used **24–41
microseconds**. That write repeated the last acknowledged output. These control
requests did not overlap. Healthy SDK traces also issue cooler writes **26–63
microseconds** after another control write completes, so a universal delay is
not justified by these measurements. Reducing repeated output traffic avoids
many unnecessary transactions, but neither spacing nor the refresh policy
establishes the cause of the remote outage.

Implementation review also identified edge cases that belong in acceptance:

- A heater enable flag alone does not establish its output preset. Cache the
  acknowledged preset separately, and forget it if its compound flag update
  fails.
- A valid error reply from a living supervisor does not mean its IPC is broken.
  Preserve the supervisor and its outage state even when the error is not
  eligible for exposure replacement.
- The frontend deadline must cover the inner command, readback, environment
  refresh and bounded worker retirement. Do not extend the USB deadline.
- Cached temperature and power must remain finite while control availability
  explicitly reflects an outage discovered during idle polling.
- Lost acknowledgements for all persistent camera settings must stop pre-arm
  replay, including heater, fan and LED settings.

The October 12 recovery validation, before adding the periodic keepalive, passed:

- **1,101 Rust workspace tests**, **127 managed core tests**, and **561 NINA
  tests**. Three unrelated ASI585/registered-COM opt-ins were skipped. The NINA
  run included the attached ASI6200's full-frame direct/SDK repeats, logging
  modes and cancellation/restart tests.
- **28 physical control captures**, 14 per backend, at 9576 × 6388 RAW16 bin 1
  and five seconds. Hashes, every row, one exposure start per frame, actuator
  state and transport reuse were checked. The direct path needed no recovery,
  reopen or sensor initialization after its first frame.
- **All 11 physical cancellation stages**, followed by 11 fresh full frames,
  plus two full-frame freshness baselines. Stop acknowledgements ranged from
  **0.016 to 0.344 seconds**. The worker and thermal settings survived each
  cancellation; physical BC19 readback confirmed cooler/heater-off at close.
- Clippy with warnings denied, formatting, both generated contracts, native
  camera IPC, simulated SDK/direct USB escalation, shared JavaScript configuration
  tests and a real headless browser edit/save pass.

| Physical hold | Temperature target/range | Feedback reads | Cooler writes | Repeated encoded outputs |
| --- | --- | --- | --- | --- |
| Direct, 30.89 seconds | 20°C / 19.4–20.6°C | 30 | 14 | 0 |
| SDK, 30.17 seconds | 18°C / 17.6–18.6°C | 15 | 3 | 0 |

Direct and SDK use different regulators and feedback cadences. These holds
validate their behavior; they are not equivalent thermal benchmarks. Median
five-second image delivery was 6.360 seconds direct and 6.282 seconds SDK.
Repeated enabled cooler/heater requests caused no direct actuator-flag or heater
preset writes. Explicit off and initialization/reconnect restoration remain
intentional forced writes.

Those binaries match the hashes recorded by physical acceptance. Results,
TRX reports, command traces, frame summaries and hashes are retained privately
under `artifacts/camera-control-recovery-20261012/`; `summary.json` indexes the
measurements. Traces include device identities and are not release assets.

The remote USB outages were not reproduced. Deterministic fixtures establish
the new timeout/recovery behavior; healthy Windows hardware establishes normal
capture, thermal control and cancellation behavior. Mandatory clock restoration
and terminal I/O drain can still wait on their bounded deadlines. Linux physical
reset and port-power behavior were not retested in this pass. The external
watcher's memory issue remains work for its owning plugin.

## Periodic keepalive acceptance

The follow-up policy retains unchanged cooler output refreshes after five
seconds since the last successful write. The complete ZWO crate suite passed
**200 tests**, including deterministic FPGA/DAC timing, forced restoration and
uncertain-write regressions. Clippy with warnings denied, formatting and diff
checks passed.

The amended direct/SDK hardware matrix passed **34 full-frame captures**:
20 direct and 14 SDK. During the dedicated 34.828-second enabled, zero-demand
window, direct mode sent six acknowledged cooler writes with unchanged output;
successive gaps were **5.079–5.285 seconds**. It made **34 temperature reads**
with a maximum gap of **1.216 seconds**. Changing outputs during cooldown were
written as little as **1.002 seconds** apart, independently of the keepalive.
Required exposure-clock transitions preserved cooler/heater bits and did not
rewrite the heater preset. Healthy captures needed no reopen or sensor
initialization after the first frame; both backends exited cleanly with cooler
and heater off, confirmed by physical BC19 readback.

Results and the tested worker hash are indexed by `summary.json` under
`artifacts/camera-control-keepalive-20261012/`; `controls-final/results.json`
contains the complete matrix. The tests establish cadence and normal operation
on the attached healthy camera, not the unknown firmware timeout or the cause
of the remote USB outage.

## Prepared USB helper and NINA peripheral recovery

Windows camera USB recovery now uses a helper prepared with elevation before
imaging. Runtime recovery never launches UAC. Preparation approves the selected
camera for the current Windows user/session for at most 24 hours; connection
checks that approval before imaging. The setup shows a note that cycling camera
USB can disconnect attached peripherals. There is no acknowledgement checkbox.
The helper requests the camera's downstream port, verifies its identity and
arrival generation, and reports observed USB inventory changes. That inventory
cannot establish every brief disconnect or guarantee electrical isolation.

The NINA camera can reconnect previously connected equipment that reports a
disconnect within a configurable window after its USB reset starts. The default
is enabled with a **15-second** window, adjustable from **1 to 60 seconds** at
the next camera connection. Correlation uses the supervisor's recovery ID and
timestamp, including when NINA reports a peripheral disconnect before the
asynchronous camera log arrives. Devices already disconnected, late unrelated
disconnects, and changed equipment selections or profiles are not reconnected.

Replacement exposures wait until the window closes and the affected equipment
passes bounded NINA disconnect/rescan/connect operations. An unresolved NINA
operation cannot overlap another attempt, even after a camera reconnect creates
new adapters. Filter wheel, focuser, rotator and flat-panel state must be known
and unchanged; Regain does not move equipment to restore state. A failed or
uncertain pass blocks subsequent captures until an explicit camera disconnect
and reconnect. Guiding and switch outputs are not restarted or restored. NINA
can only report what its equipment drivers expose; a live PHD2 connection does
not prove its USB guide camera remained connected.

The final software validation passed:

- **167 managed Core tests** and **586 NINA tests**. Five explicit hardware/COM
  opt-ins were skipped. Simulated SDK/direct USB recovery through the camera
  plugin confirmed that image readiness waits for peripheral acceptance and a
  changed filter blocks both replacement and subsequent captures.
- Complete Rust Core, Alpaca and transport suites after the peripheral gate was
  added, including the Hub close/connect lifecycle regression test. Whole
  workspace, all-target/all-feature clippy passed with warnings denied.
- **31 bounded non-admin helper process checks**, including a real, inert named
  pipe fixture rejected because its server was not elevated. The checks did not
  prepare a helper, request UAC, or access physical hardware.
- Native SDK/direct camera IPC, worker-termination USB recovery simulations,
  both generated configuration contracts, formatting and the net48 ASCOM build.

Logs, TRX files, process-check evidence, binary hashes and the validation index
are retained privately in `artifacts/usb-helper-20261012/`. At that point, the
elevated helper's prepare/reset/stop lifecycle, physical collateral peripheral
reconnection, and the new installer busy-process fixture had not been exercised
locally. The following acceptance covers the new helper binary; the earlier
capture/thermal matrices retain their own recorded worker hashes.

## Physical Windows USB cycle acceptance

The attached ASI6200MM Pro completed the prepared-helper lifecycle on October
12, 2026. No NINA or guiding session was running. An unelevated client explicitly
prepared an elevated helper, checked its approval, and cycled the selected
camera's downstream USB port. The native cycle completed in **210 milliseconds**;
the camera returned with the same verified serial and a new Windows arrival
generation. An old-generation reset request was rejected, and a subsequent
identity check proved that rejection had not caused another cycle. Repeating
preparation reused the approval without extending its original expiry.

Both direct and SDK Alpaca paths then completed the full physical matrix:

- Four **9576 × 6388 RAW16 bin-1 five-second images per backend**: a baseline,
  a replacement after terminating only the owned camera worker, and two fresh
  images on the recovered worker. All four pixel hashes differed per backend;
  every image contained the expected **122,342,976 bytes** of pixels.
- Exactly one automatic USB cycle per injected worker failure, **208
  milliseconds** in each backend. The same camera serial returned with a fresh
  arrival generation, retained helper approval, and a new worker that remained
  stable for the subsequent captures. The entire pass used three physical
  cycles, including the initial manual cycle.
- Helper Stop succeeded; a subsequent unelevated status reported it unprepared.
  Camera close succeeded, and an independent passive USB trace confirmed
  physical **BC19 = 0x80**, with both cooler and dew heater off.

Replacement-image delivery took **14.437 seconds direct** and **10.672 seconds
SDK** in these debug Alpaca builds, including the replacement five-second
exposure, validation and image transfer to the test client. Those are separate
from the native port-cycle timing and are not release performance measurements.

USB inventory reported only the ASI6200 reenumerating during all three cycles;
the attached ASI662 was not observed changing. No NINA peripherals were connected
for this pass, so collateral peripheral reconnection remains covered by the
software fixtures rather than physical acceptance. The installer busy-process
fixture and physical Linux reset/power controls also remain unvalidated locally.

The generalized hardware harness additionally passed its complete direct/SDK
simulation matrix. Evidence, persistent server logs/profiles, image hashes,
USB inventories, final thermal readback and tested binary hashes are indexed
by `artifacts/usb-cycle-physical-20261012/summary.json`. These private artifacts
contain device identities and are not release assets.

## Installed NINA and vendor CAA acceptance

The October 12 pass used installed NINA **3.2.0.9001**, the current Release
Regain plugin build, direct ASI6200MM Pro mode, and ZWO's official ASCOM CAA
driver (`ASCOM.ASICAA.Rotator`). The CAA was connected before the camera and
remained stationary at **90 degrees**, reverse off. NINA setup explicitly
prepared the elevated helper before connecting the camera. No elevation was
requested during either recovery. Both devices were disconnected and helper
Stop was verified at the end; camera close logged cooler and dew-heater disable.

The acceptance review fixed two NINA lifecycle gaps: discovering the chooser
identity when a peripheral was already connected, and waiting for the rotator's
first complete position poll after reconnect instead of comparing its temporary
zero mechanical position. The complete NINA suite then passed **590 tests**, with
five explicit hardware/COM opt-ins skipped; the Release build had no warnings or
errors. These fixes were included in the installed build used below.

- A normal camera USB recovery completed in **210 ms**. NINA did not report a
  disconnect for the CAA on the camera's USB 2 hub. Regain waited the full
  **15-second** observation
  window before starting the replacement five-second exposure. Two subsequent
  full-frame captures succeeded on the replacement worker.
- A second recovery completed in **217 ms**. A separately prepared, fixed-target
  Windows fixture attempted to disable only the CAA USB devnode during the
  observation window. ZWO's ASCOM driver reported loss at **13:55:48.733**;
  NINA's own reconnect failed. At **13:55:58.551**, Regain invoked NINA's
  disconnect/rescan/connect path. The same ASCOM identity connected at
  **13:56:01.017**. After a fresh position poll, matching optical state was
  accepted at **13:56:03.031**, the peripheral gate opened at **13:56:03.033**,
  and the native replacement exposure started at **13:56:03.658**. Its FITS was
  saved at **13:56:11.215**. Two further full-frame captures succeeded.
- All **seven** saved FITS files were **9576 × 6388**, RAW16, bin 1, **five
  seconds**, with `INSTRUME = ZWO ASI6200MM Pro`. Each contained **122,342,976
  pixel bytes**, and every pixel hash differed. Each injected worker termination
  caused exactly one camera cycle; the supervisor stayed alive.

The peripheral fixture is **not a clean disable/hold/enable pass**: Windows
returned **3010 (reboot required)** for disable and **50 (pending reboot)** for
enable, although the vendor SDK detected actual connection loss and Regain
successfully restored it. After orderly NINA disconnect, a fixed-target SetupAPI
enable cleared `CONFIGFLAG_DISABLED` from 1 to 0 and confirmed `ProblemCode = 0`.
The CAA remained accessible at the same serial and position. Windows retained
`IsRebootRequired = true`; no reboot was performed. This limitation is retained
separately from the successful installed NINA reconnection and capture-gate
acceptance. The test does not establish that a camera USB 3 reset naturally
disconnects its USB 2 peripherals.

Private evidence is indexed by `artifacts/nina-caa-usb-20261012/summary.json`:
copied NINA logs, unique recovery IDs and timings, FITS headers and pixel hashes,
installed binary hashes, full-suite TRX, official installer signature metadata,
fault-fixture results, cleanup status and final CAA readback. Physical Linux
reset/power controls and the installer busy-process fixture remain unvalidated
locally.

## USB recovery toggle and session lifetime

The shared NINA and native ASCOM camera setup now exposes **Automatic USB
reset** with a positive failure threshold, instead of a separate numeric
zero-disables control. Enabling explicitly prepares the selected camera's
elevated helper, then saves the policy immediately. Denied preparation does
not enable it. Disabling saves zero first and revokes only the selected
model/serial's approved physical bindings; other camera instances retain their
approval. Removing the final binding stops the helper.

The elevated process is independent of the camera worker, supervisor and
frontend, with no arbitrary 24-hour expiry. It lasts for the current Windows
user/session until disabled, sign-out or reboot. A saved enabled policy still
requires explicit preparation after a new login; automatic recovery never
requests elevation. Every recovery retains its bounded deadline, authenticated
same-user/session pipe and live device-generation verification.

The complete NINA suite passed **601 tests**, with five existing explicit
hardware/COM opt-ins skipped. Native transport passed **13 tests**, and all
**36** non-admin helper process checks passed, including rejection of a real
unelevated pipe squatter for status and revocation. Native ASCOM built for
.NET Framework 4.8 with no warnings or errors. Genuine absent-helper status
and revocation are idempotent; authentication and protocol failures remain
errors. Evidence is under `artifacts/usb-helper-20261012`.

Installed NINA setup was checked with the selected ASI6200MM Pro: the shared
skinned toggle saves OFF immediately without closing the dialog, and status
correctly reports a stopped helper. After interactive Windows approval, helper
PID 3820 remained authenticated and ready through NINA exit and restart. The
persisted policy remained enabled even though setup had been canceled. The
first camera reconnect exposed the supervisor's old numeric-expiry preflight;
its validator was updated to accept the explicit session lifetime while
retaining legacy finite expiry checks and rejecting malformed lifetime data.

Leaving that prompt unanswered exposed a setup deadline gap: killing the
unelevated CLI did not release redirected pipe reads retained by a descendant.
The managed runner now races completion against an independent deadline and
keeps reader/process disposal off the caller's thread. It kills only its owned
CLI, never the elevated helper or its process tree. A real process fixture
holds both output pipes open after its parent exits and checks timeout and
caller cancellation while the child remains alive. The elevated native server
also detaches its standard handles after authenticated bootstrap, so its
session lifetime cannot hold the setup client's pipes open.

The final installed NINA pass used the direct ASI6200MM Pro and ZWO CAA ASCOM
driver. Exactly one acknowledged `USB_HUB_CYCLE_PORT` completed in **215 ms**
after an intentionally terminated camera worker. The CAA ASCOM driver reported
connected at **90 degrees** throughout its two-second polling observations;
Regain did not invoke a CAA reconnect. The replacement exposure started only
after the complete 15-second peripheral observation window. All **nine** saved FITS images were
**9576 × 6388**, five seconds, RAW16, bin 1, with **122,342,976 pixel bytes**
and distinct SHA256 pixel hashes. Seven images covered the baseline, replacement
and fresh captures; two further images succeeded after normal camera disconnect
and reconnect created supervisor **29704** in place of **22976**.

The same elevated helper **PID 3820**, identified by its creation timestamp,
survived two NINA exits and the supervisor restart without another approval
prompt. Camera disconnect disabled cooler and heater controls. With both camera
and CAA disconnected, turning the setup toggle OFF persisted zero immediately,
revoked the final camera binding and ended the helper. Canceling setup retained
that OFF policy. Camera USB recovery required no Windows restart. This pass did
not inject a CAA disconnect; the earlier separately qualified test records its
actual reconnect behavior.

Read-only topology inspection after this pass confirmed separate Windows USB
paths for the camera's USB 3 function and the CAA's USB 2 hub. The camera's last
arrival changed to **15:08:06.342**, while the CAA's remained **13:28:47.245**.
The helper's before/after inventory observed only the camera reenumerating.
These snapshots and driver polls do not exclude a brief physical interruption
between observations. This pass therefore establishes no observed CAA disconnect,
not uninterrupted physical connection or an unplug-equivalent cycle of the
camera and its internal hub.

Core validation passed **81 unit and six integration tests**. Private hardware
evidence is indexed by `artifacts/usb-session-toggle-20261012/summary.json` and
`hardware-summary.json`, including full image hashes, recovery ordering, helper
creation timestamps, installed binary hashes and cleanup status. The physically
elevated helper used the session-lifetime build before standard-handle detachment;
the final detachment build was installed after cleanup. Its pipe-release behavior
is covered by the real child-process fixtures, rather than another elevated
hardware pass.

## Optional broader USB recovery

Keep the verified camera-port cycle as the default. The camera enclosure includes
its peripheral hub, but Windows exposes the SuperSpeed imaging device and USB 2
hub as separate logical paths. The current implementation cycles the selected
imaging device's port on its immediate parent hub; it does not explicitly cycle
the camera's USB 2 hub or the parent hub's upstream connection.

Consider two additional, explicitly selected scopes. First, resolve the camera
connector's USB 2 companion port with
`IOCTL_USB_GET_PORT_CONNECTOR_PROPERTIES`, then verify the attached camera hub and
capture its descendants. This can cover the internal hub without resetting the
entire external hub. Second, an upstream-hub cycle could recover a stuck external
hub, but may affect unrelated devices. The attached system's parent hub already
has another downstream hub, so an upstream cycle cannot assume camera-only scope.
Use discovered topology rather than model-specific hub IDs or equal port numbers.

Neither broader scope is implemented or physically validated. Before adding one,
extend explicit helper preparation to bind its exact hub/connector, topology and
affected devices; retain per-operation deadlines, cancellation cleanup and the
per-outage reset limit. Log the chosen scope and observed removals/arrivals, then
hold replacement captures behind the existing peripheral reconnect/state gate.
Hardware acceptance must observe a real CAA disappearance and return, verify the
same vendor ASCOM selection and optical state, and finish with full-frame captures.
A USB cycle still cannot guarantee a cold restart while external 12 V remains on.
