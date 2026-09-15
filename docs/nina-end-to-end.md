# NINA end-to-end acceptance matrix

This document records successive hardware validation runs. The initial matrix
below and later ASI2600 sections predate ASI6200 support; see the final ASI6200
section and [model-specific evidence](asi6200-p25.md) for the new P25 tests.

Initial status: **attached-hardware acceptance matrix passed**, 2026-09-14. All 11
advertised camera/backend/bin combinations produced fresh images inspected in
NINA's image pane. ROI, timing, controls, sequence metadata, cancellation and
worker-recovery checks below also passed. This covers SDK operation on the three
attached sensors and experimental direct operation on ASI676MC; it does not
establish P25 compatibility or recovery from physical USB faults.

Preflight confirmed that the installed `ZwoGain.NINA.dll` matches the local
Release build by SHA-256. The guide-offset correction `ed3a820` was subsequently
built, installed and tested in NINA; GitHub CI passed through `81c1d96`.
SDK property enumeration, which does not open the cameras, confirmed the
binning modes below.

## Scope and expected full-frame output

The plugin exposes RAW16 only, even when the SDK supports other formats.
Live view is not implemented. Test every advertised binning mode through the
plugin, including bin 3; the older tracing harness only accepts bins 1, 2 and 4.
NINA's adapter rounds output width down to a multiple of 8 and height to even.

| Backend | Camera | Bin | Expected image-pane dimensions | Result |
| --- | --- | --- | --- | --- |
| SDK | ASI676MC | 1 | 3552 × 3552 | Pass: visible new frame, 16-bit, mean 575.06 |
| SDK | ASI676MC | 2 | 1776 × 1776 | Pass: visible new frame, 16-bit, mean 591.78 |
| SDK | ASI676MC | 3 | 1184 × 1184 | Pass: visible new frame, 16-bit, mean 576.18 |
| SDK | ASI676MC | 4 | 888 × 888 | Pass: visible new frame, 16-bit, mean 576.96 |
| SDK | ASI2600MM Duo | 1 | 6248 × 4176 | Pass: visible new frame, 16-bit, mean 1.73 |
| SDK | ASI2600MM Duo | 2 | 3120 × 2088 | Pass: visible new frame, 16-bit, mean 1.34 |
| SDK | ASI2600MM Duo | 3 | 2080 × 1392 | Pass: visible new frame, 16-bit, mean 1.23 |
| SDK | ASI2600MM Duo | 4 | 1560 × 1044 | Pass: visible new frame, 16-bit, mean 1.21 |
| SDK | ASI220MM Mini | 1 | 1920 × 1080 | Pass after offset fix: mean 3196.29, offset 200 |
| SDK | ASI220MM Mini | 2 | 960 × 540 | Pass after offset fix: mean 3182.31, offset 200 |
| Direct | ASI676MC | 1 | 3552 × 3552 | Pass: visible new frame, 16-bit, mean 630.14 |

Completed rows used one second, gain 0, requested offset 0, USB limit 40, with
Save and Loop disabled. The guide SDK applied offset 200 as described below.
The ASI676MC produced an illuminated scene; the user's cap description applies
to the Duo. Matrix snapshots were unsaved; sequence FITS files stayed local
outside the repository. No image pixels were committed.
The image-pane dimensions and statistics above were read from screenshots.

To repeat each row, select the camera/backend in setup, save, connect, capture a
one-second frame and inspect the newly displayed image. Check camera
name, backend, binning, dimensions, 16-bit output, exposure metadata and image
statistics. Use automatic stretching to inspect dark-frame structure. A dark
image is expected; absence of stars is not a failure. Distinguish actual zero
pixels, clipping, stale images, truncation and malformed edges from display
stretch behavior. Capped frames cannot establish illuminated Bayer orientation.

## Additional modes and transitions

### Completed Duo recovery checks

- Cooling initially off, dew heater off, power 0%, sensor 29.9°C; NINA's
  cooling target field was −10°C. Enabled a modest 27°C target through NINA.
- A ten-second bin-4 capture with cooling enabled displayed a new frame
  (mean 1.93). The first deliberate worker kill landed after download, so it
  counts only as an idle-worker failure. The subsequent request reconnected
  after five seconds, restored cooling, settled near the prior 27.5°C for
  about 28 seconds, and displayed a new frame (mean 1.76).
- A second worker kill occurred about one second into an active ten-second
  exposure. NINA logged attempt 1/4 failing in `Exposing`, waited five seconds,
  reopened the same serial, restored cooling near 26.3°C, collected the three
  stability samples over four seconds, and repeated the exposure. A new
  1560 × 1044 image appeared (mean 1.80), with cooler enabled and target 27°C.
  No capture-error notification appeared for this recovered request.
- A 31-second request logged retries disabled by the default 30-second cutoff.
  Killing its worker during exposure produced `Attempt 1/1` and an error
  notification; no replacement image or automatic retry occurred.
- Canceling the next request during cooling recovery returned the snapshot
  button promptly. The following one-second request reconnected and displayed
  a fresh 1560 × 1044 image (mean 1.15). This verifies cancellation during
  recovery; cancellation during a running exposure remains a separate case.
- After these checks, NINA's warming operation disabled the cooler; dew heater
  stayed off, power was 0%, and the original −10°C target field was restored.

### Guide-sensor offset issue discovered in NINA

The initial guide-sensor capture failed before exposure: the SDK advertised
offset 0–1500 but read back 200 after NINA requested 0. The strict check caused
four futile attempts. A bounded SDK probe confirmed offsets 0, 1, 50, 100 and
199 all become 200 at gains 0, 100, 300 and 600; offsets 200, 201, 250, 500 and
1500 read back exactly. Original controls were restored after the probe.

The supervisor now accepts SDK offset normalization only within the advertised
range, logs the adjustment, and retains the applied value for recovery, NINA
properties and frame metadata. Other control read-back mismatches still fail,
including cooling. Tests cover offset normalization through transfer recovery,
out-of-range read-back rejection, strict cooling restoration, and NINA metadata.
After installing `ed3a820` and restarting NINA, both full-frame binning modes
passed and image metadata showed offset 200. A 512 × 256 sensor ROI at (16, 32)
and bin 2 produced 256 × 128 (mean 3205.43); disabling subsampling restored
960 × 540 (mean 3182.75). All four were fresh one-second images with gain 0,
16-bit output, and no capture errors.

Additional guide timing checks at bin 1, gain 0, applied offset 200 and USB 40
displayed fresh 1920 × 1080 images at 32 µs (mean 3182.45, standard deviation
28.62) and the advertised ten-second maximum (mean 3209.76, standard deviation
72.47). At 100 ms, ROI (16, 32, 512, 256) produced 512 × 256 (mean 3176.25,
standard deviation 28.28). Disabling ROI restored 1920 × 1080 (mean 3178.96,
standard deviation 26.75). No capture errors occurred.

At 100 ms and full-frame bin 1, USB 100 transferred frames at gain 300 /
offset 750 (mean 12507.50, standard deviation 217.68) and gain 600 / offset
1500 (mean 46572.27, standard deviation 6186.46), with matching metadata.
Restoring requested gain/offset 0 and USB 40 produced applied offset 200 and
the original baseline (mean 3178.88 versus 3178.96, standard deviation 26.78).

### Additional SDK ASI676MC checks

With installed implementation `ed3a820`, the legacy sequencer produced a new
32 µs Bias full frame (3552 × 3552, mean 43.01, standard deviation 21.24).
Canceling a ten-second snapshot after 9.7 seconds logged `Aborted`, returned
the capture button promptly, and did not replace the image. The next 100 ms
capture reconnected and displayed a new full frame (mean 94.96).

At 100 ms, gain/offset 0 and USB limit 40, the sensor ROI (16, 32, 512, 256)
produced 512 × 256 at bin 1 (mean 43.78, standard deviation 21.40) and
256 × 128 at bin 2 (mean 48.66, standard deviation 10.88). Disabling the ROI
restored full-frame bin 2 output, 1776 × 1776 (mean 94.88). Each was a fresh
image visibly inspected in NINA, without capture errors.

The SDK legacy sequence also displayed fresh one-second Dark (mean 565.86,
standard deviation 1421.46) and Light (mean 568.99, standard deviation 1418.78)
full frames. All three SDK sequence FITS headers matched the requested type,
duration, 3552 × 3552 dimensions, RAW16, bin 1, gain/offset 0 and RGGB pattern.
These sequence images remain in the dedicated local test directory only.

At 100 ms and full-frame bin 2, USB limit 100 transferred new frames with
gain 300 / offset 100 (mean 9330.82, standard deviation 4467.40) and the
advertised maxima, gain 600 / offset 200 (mean 58595.36, standard deviation
5942.12; substantial expected clipping in the illuminated scene). Metadata
matched both requested control pairs. Restoring gain/offset 0 and USB 40
returned the baseline response (mean 94.74 versus 94.88 before changes).
A normal 31-second SDK exposure also completed at full-frame bin 2 (mean
11658.19, standard deviation 18150.12). The 30-second retry cutoff did not
prevent the exposure; it only disables automatic retries for that request.

### Additional SDK Duo-main checks

Installed implementation `ed3a820` captured the advertised minimum 32 µs at
full-frame bin 2, 3120 × 2088 (mean 1.19, standard deviation 1.42).
At 100 ms, gain/offset 0 and USB 40, the ROI (16, 32, 512, 256) produced
256 × 128 at bin 2 (mean 1.15, standard deviation 1.43) and 512 × 256 at
bin 1 (mean 1.47, standard deviation 2.89). Disabling subsampling restored
6248 × 4176 (mean 1.52, standard deviation 2.93). All were fresh capped-camera
images inspected in NINA. Cooling and dew heater remained off.

Full-frame 100 ms control tests also passed. Equipment default gain -25 with
snapshot gain -1 (use the camera default) produced metadata gain -25, offset 0
(mean 1.34, standard deviation 2.42). At USB 100, gain 350 / offset 120 yielded
mean 1192.42, standard deviation 91.20; the advertised maxima 700 / 240 yielded
mean 2942.65, standard deviation 3663.31. Metadata matched both control pairs.
Restoring gain/offset 0 and USB 40 returned the baseline (mean 1.53 versus
1.52 before changes, standard deviation 2.93). No capture errors occurred.

### Completed direct-driver checks

- Installed implementation `ed3a820` produced a fresh 3552 × 3552 image at
  one second, gain/offset 0 (mean 630.14, standard deviation 1504.94).
  The direct worker had no ASI SDK DLL loaded. Equipment advertised bin 1,
  32 µs–30 s, no temperature reading, and no writable USB bandwidth control.
- Killing the direct worker about one second into a ten-second exposure logged
  failure in `Exposing`, waited five seconds, reopened the same serial in a
  new direct worker, and repeated the exposure. NINA displayed a fresh full
  frame (mean 5193.16) without a capture-error notification.
- Canceling a separate ten-second exposure after about six seconds returned
  the capture button promptly and logged `Aborted`. The following 100 ms
  request reconnected and displayed a fresh full frame (mean 108.22).
- At 100 ms, the ROI (16, 32, 512, 256) displayed 512 × 256 (mean 52.04,
  standard deviation 28.76). The minimum 64 × 64 ROI at the same origin also
  produced a new image (mean 51.27, standard deviation 28.51).
- A 32 × 64 ROI was rejected immediately with the minimum-size/alignment
  requirements and no retry loop. The next valid request reopened the worker
  after the configured delay and restored full-frame output.
- Additional full-frame timing cases passed at gain/offset 0: 32 µs (mean
  51.28), 10 ms (56.73), 0.999999 s (623.98), and 2 s (1202.65). Each produced
  a new image in NINA; the 32 µs image was predominantly read noise as expected.
- The 30 s maximum produced a new full frame (mean 12499.81). A 31 s request
  failed immediately as outside camera capabilities; no new exposure or image.
- At 100 ms, gains 179, 180 and 600 produced new full frames with matching
  metadata (means 883.90, 840.38 and 55182.97 respectively). The illuminated
  scene clipped heavily at gain 600. Gain 0 / offset 200 produced a new frame
  with matching metadata (mean 15408.17). Restoring gain/offset 0 returned the
  baseline image response (mean 108.25 versus 108.22 before control changes).
- A later 100 ms intermediate-offset check at gain 0 / offset 100 displayed
  a fresh 3552 × 3552 frame (mean 7757.34, standard deviation 1840.73) with
  matching metadata. Restoring offset 0 produced mean 107.49, standard
  deviation 152.03, close to the earlier baseline.
- Setup rejected the saved guide sensor when the experimental option was
  enabled, with an inline instruction to choose ASI676MC or disable that
  option. Selecting ASI676MC then connected successfully using the direct
  backend. The Duo-main selection was separately rejected by the same guard;
  disabling the experimental option then connected the Duo through the SDK.
- The legacy sequencer produced fresh direct full-frame images for Light
  (1 s, mean 629.80), Dark (1 s, mean 629.84), and Bias (1 s, mean 629.76;
  then 32 µs, mean 51.30). Each saved FITS header had the requested frame type,
  duration, bin 1, gain/offset 0, RGGB pattern and 3552 × 3552 dimensions.
  Legacy NINA preserves the entered Bias duration, so minimum-duration Bias
  requires entering 32 µs explicitly. ASI676MC has no shutter; the illuminated
  scene remains visible in a one-second Dark/Bias request.
  Sequence files are kept only in a dedicated local test directory, outside
  the repository. Snapshot Save remains off.

The coverage checklist below is complete for the attached hardware and exposed
backends. Sequence and cancellation cases are per backend, not every possible
combination of camera, controls and exposure type. The results above record
settings, visible image dimensions, statistics and capture/recovery outcomes.
Keep raw logs and screenshots containing device identity local; publish sanitized
results, never camera pixels or calibration payloads without a separate request.

| Coverage | Cases / acceptance |
| --- | --- |
| Camera/backend persistence | Reopen setup after saving each choice; disconnect/reconnect; verify no silent backend or camera substitution. Leave SDK selected after testing. |
| Subframe | Each camera/backend at bin 1: 512 × 256 with an aligned nonzero origin; return to full frame and verify dimensions recover. Test binned ROI on SDK cameras too. |
| Direct minimum ROI | 64 × 64 at a valid even origin; smaller unsupported ROI must fail clearly without repeated recovery attempts. |
| Exposure timing | Direct: 32 µs, 10 ms, 100 ms, 0.999999 s, 1 s, 2 s and 30 s. SDK cameras: advertised minimum, short, 1 s and long exposures. Check a new image after each. |
| Gain / offset | Read advertised ranges; exercise valid extrema and a representative middle value. ASI676 direct additionally crosses gain 179/180. Restore initial controls. |
| Bandwidth | SDK: capture at supported low/high USB limits and restore initial value. Direct: fixed 40, with no writable bandwidth control. |
| Repeated snapshots | At least three consecutive short captures per camera/backend; verify completion and changing capture timestamps. |
| Sequence paths | Run a small NINA sequence using Light, Dark and Bias frame types through each backend; verify exposure/bin/gain metadata and image-pane results. Keep image saving disabled where possible or use an explicit local test directory. |
| Cancellation | Abort a multi-second exposure on each backend; verify prompt cancellation, then successfully capture another image. |
| SDK process recovery | Terminate only the plugin's SDK worker during a short capture; NINA should retain the request, reconnect after its delay, restore controls and display the replacement image. |
| Direct process recovery | Terminate only the direct worker during a short capture; recover on the direct backend and display the replacement image. |
| Retry cutoff | SDK exposure above 30 s with deliberate host termination must fail after one attempt with the default cutoff. Direct ASI2600/6200 accept up to 2,000 s; retained reads remain permitted, while full recapture still obeys the cutoff. ASI676 and guide retain their 30 s and 10 s direct limits. |
| Duo cooling | Record initial temperature/target/enable/power. Exercise a modest target change and enabled/disabled states; capture during cooling. During a short-exposure worker failure, verify target/enable restoration and settling near the prior temperature before retry. Restore initial state. |
| Unsupported direct cameras | Models/interfaces outside the explicit direct allowlist must be rejected clearly. The tested ASI6200MM Pro P25 now has its own path. Confirm the SDK remains usable after an unsupported direct selection. |

Do not infer cable-reconnect retention or natural USB-error behavior from a
process-kill test. Physical detach/power-cycle experiments are separate cases.
NINA end-to-end success requires an observed new image in its image pane; the
earlier command-line and simulator results remain separately documented in
[validation](validation.md).

## Restored state after the earlier ASI676 matrix

NINA was left connected to ASI676MC through the SDK, gain/offset 0 and USB 40.
A final one-second, bin-1 full frame displayed 3552 × 3552, mean 563.85 and
standard deviation 1405.15, with matching gain/offset metadata. The entire
image was fitted to the image pane. Snapshot Loop, Save and subsampling were
off. The original NINA image output directory was restored; sequence test
files remain in its separate local `ZWOgain-e2e` subdirectory. Duo cooling and
dew heater were off, with its original −10°C target field restored before
disconnecting that camera.

## Issue found during visual testing

NINA's CheckBox theme rendered only ON/OFF and hid the experimental backend
toggle's content. The setup dialog now places its descriptive label in a
separate TextBlock. The correction was installed and visually verified after
restarting NINA. The camera picker selected and persisted the Duo correctly.

The interruption returned `IGraphicsCaptureItemInterop.CreateForMonitor failed:
Could not capture the given monitor. (0x80070057)`. A fresh JavaScript/Computer
Use connection did not resolve this second error. Restoring the RDP desktop
resolved it, and the bin-4 capture subsequently passed.

## Duo direct backend and SDK fallback integration (2026-09-14)

The updated plugin was installed in NINA 3.2.0.9001 and tested interactively
with both capped ASI2600MM Pro Duo sensors. Each row below was verified as a
new image in NINA's image pane; statistics are NINA's displayed values. Main
captures used gain 100 / offset 50; guide captures used gain 100 / offset 200.
These extend the earlier SDK/ASI676 matrix; they do not imply every sequence,
control-extreme or cancellation case was repeated on each new direct sensor.

| Sensor / backend | Exposure / bin / region | Output | Mean / SD |
| --- | --- | --- | --- |
| Main / direct | 1 s / 1 / full | 6248 x 4176 | 500.43 / 13.71 |
| Main / direct | 1 s / 2 / full | 3120 x 2088 | 500.05 / 6.70 |
| Main / direct | 1 s / 3 / full | 2080 x 1392 | 499.94 / 4.11 |
| Main / direct | 1 s / 4 / full | 1560 x 1044 | 499.87 / 3.02 |
| Main / direct | 1 s / 4 / physical ROI 512 x 256 at (16, 0) | 128 x 64 | 499.83 / 1.59 |
| Main / SDK fallback before exposure | 31 s / 4 / full | 1560 x 1044 | 507.59 / 12.06 |
| Guide / direct | 0.1 s / 1 / full | 1920 x 1080 | 3204.97 / 65.84 |
| Guide / direct | 0.1 s / 2 / full | 960 x 540 | 3202.45 / 35.46 |
| Guide / SDK fallback after worker failure | 5 s / 2 / full | 960 x 540 | 3197.82 / 47.34 |
| Main / primary SDK, final verification | 0.1 s / 1 / full | 6248 x 4176 | 499.92 / 6.04 |

The 31-second main request switched to SDK before starting an exposure because
it exceeded the verified direct range. NINA logged that automatic exposure
retries were disabled above the default 30-second cutoff; the successful SDK
exposure was the first exposure attempt, not an exception to that cutoff.

For guide recovery, only NINA's identified direct worker was terminated during
the five-second exposure. The request stayed active, recorded one failed attempt,
waited five seconds, opened the SDK on the same serial, restored controls, and
displayed the replacement frame without a NINA capture error. Total elapsed time
was about 11.7 seconds. The equipment pane showed `[sdk fallback]`, gain 100,
offset 200 and USB limit 40. This hardware check includes the fix in `9dc40a6`
that carries fixed direct USB bandwidth into the writable SDK control.

The setup dialog was visually checked after restart: camera selection, serial,
direct-driver choice and SDK-fallback choice persisted. Changing sensor cleared
the previous sensor's serial, and connecting learned the new serial. The dialog
fits the desktop and scrolls to its Save button; both experimental labels remain
visible under NINA's theme. Main cooling/dew and both sensors' binning/ranges were
advertised correctly. Cooling during acquisition and temperature settling after
main-worker failure were tested separately through the plugin protocol, as
recorded in [Duo capture](duo-capture.md) and its sanitized hardware evidence.

Final state: NINA is connected to Duo main through the primary SDK, with both
experimental switches off and its serial persisted. Gain is 100, offset 50,
USB limit 40, snapshot duration 0.1 s and binning 1. Loop, Save, subsampling,
cooling and dew are off. The target-temperature field remains -10 C. The final
dark frame is fitted in the image pane. No changes were made to the image output
directory during this matrix.

The tested production revision passed 41 core tests, 10 NINA contract tests,
Rust tests/formatting/Clippy, release-package checks and GitHub CI. These tests
do not validate P25/ASI6200 protocols, physical USB detach/power loss, or natural
capture-transfer faults. Direct RAW8/video and guide retained-frame replay are
not claimed. SDK operation remains the primary path.

## Tabbed setup and loaded-cooler recovery (2026-09-14)

The setup now separates Camera, Recovery, Cooling and Advanced settings. Save
and Cancel remain outside the tabs. All four tabs were checked in the installed
NINA theme at 650 x 650: field labels, numeric values and buttons were visible
without scrolling at this size. The camera picker showed ASI2600MM Pro Duo main
and ASI220MM Mini guide as separate options. Saved serial/backend choices and
all twelve recovery fields survived a Save from the Advanced tab.

A real SDK-worker failure during active cooling exposed an early-resume issue.
Before failure the main sensor was near 3.2 C while cooling toward -10 C, with
roughly 44% output. Reopening the SDK restored target/enable but reset its
regulator to low output. Three early readings passed the old temperature-only
check; the replacement image completed, then the sensor warmed to 6.8 C while
output was still only 2%. Thermal inertia made temperature alone insufficient.

Recovery now snapshots the actual cooler power along with temperature. If power
telemetry exists, each accepted settling sample must also report at least the
prior output minus 10 percentage points (floor zero). This is a readiness check,
not a command to force cooler power. The configured temperature tolerance,
consecutive samples and recovery deadline still apply. Diagnostics record both
readings and the accepted sample count. Simulator regressions keep temperature
exactly at its prior value while dropping power: 1% and 19% block resumption
from a 30% baseline, while 20% permits it.

Temperature may also progress from its prior value toward the restored target;
it need not remain in a symmetric band around the old temperature while cooler
output ramps up. Simulator cases cover both valid further cooling toward a
colder target and rejection of cooling below that target's tolerance.

The longer real settling period also exposed NINA 3.2's independent readiness
deadline: a five-second capture failed after its exposure plus the profile's
60-second timeout even though ZWOgain was still recovering. The plugin now
temporarily extends that deadline to accommodate its bounded recovery policy,
then restores the original value after download, failure, cancellation or
disconnect. It preserves a concurrent user edit. Cancellation during a real
cooler recovery stopped the request and restored the profile value to 60 seconds.
Contract tests cover the outer deadline, restoration and concurrent edits.

The final installed build (`3fb1e30`) passed a complete loaded-cooler recovery in
NINA on the capped ASI2600MM Pro Duo main sensor. Only the identified direct
worker was terminated, during a five-second, full-frame RAW16 exposure at gain
100, offset 50 and binning 1. The fresh pre-failure snapshot was -0.3 C and 74%
cooler output, with a -10 C target. The request waited five seconds before
reopening the same camera through the SDK and restoring its controls. It then
waited for both thermal recovery and output of at least 64%. Three accepted
samples at -1.9, -2.1 and -2.3 C allowed the replacement exposure to start.

The original request began at 09:24:51.6; the replacement began at 09:27:58.9
and finished downloading at 09:28:04.7, about 193 seconds total. NINA kept the
same request active and displayed the 6248 x 4176 image without a capture error
(mean 500.16 ADU, standard deviation 8.08 ADU). The profile timeout returned to
60 seconds. A subsequent one-second image also displayed normally (mean 499.90,
standard deviation 6.03), demonstrating continued capture after recovery.

The main camera remains connected through the recovered SDK, with its cooler
enabled and target -10 C. Both experimental settings are saved off for the next
connection, and the selected main camera and serial remain persisted. Snapshot
duration is restored to one second; gain 100, offset 50, USB limit 40 and binning
1 remain selected. Dew, Loop, Save and subsampling are off. The image pane holds
the final dark frame. The tested code passed 46 core tests, 12 NINA contract
tests, Rust tests/formatting/Clippy and five release-package checks. This failure
injection validates worker restart and SDK fallback with the real cooler; it
does not establish recovery from physical USB detach or a natural transfer fault.

## ASI2600 long direct exposures (2026-09-14)

The direct ASI2600 backend's old research cap of 30 seconds prevented a
60-second request before acquisition. The camera's advertised exposure control
and Rust validation now use the SDK's 2,000-second maximum. Integrations of at
least one second already use host timing; the sensor frame/shutter register
values stay fixed while the host waits. Both the capture watchdog and readiness
deadline scale with the requested duration. The retry cutoff is unchanged.

The updated release worker was installed while NINA's camera was disconnected,
then reconnected without restarting NINA. The equipment pane reported a
2,000-second maximum with direct mode on and SDK fallback off. A full-frame
60-second RAW16 capture at gain 100, offset 50 and binning 1 began at
09:49:00.96 and finished downloading at 09:50:01.73. NINA displayed a
6248 x 4176 dark frame (mean 517.72 ADU, standard deviation 64.04 ADU).
The camera was near room temperature with the dew heater on. Its capped dark
frame includes hot pixels; NINA's star detection is not an optical measurement.

Simulator failure tests start 60-, 120-, 1,200- and 2,000-second exposures on
the direct backend, terminate the worker immediately, and verify that neither
another exposure nor SDK fallback is attempted with the default retry cutoff.
Additional checks reject requests beyond 2,000 seconds without starting the
hardware and preserve SDK routing for unsupported ROIs.

A subsequent 1,200-second full-frame exposure used the same direct worker with
SDK fallback disabled. It started at 09:50:47.84, entered download at
10:10:48.64 and returned idle at 10:10:48.67: 1,200.83 seconds total. NINA
displayed the completed 6248 x 4176 frame at gain 100, offset 50 and binning 1
(mean 867.71 ADU, median 845 ADU, standard deviation 424.03 ADU). There were no
failed attempts, worker restarts or SDK fallbacks. Process-module inspection
during and after capture confirmed that the direct worker had no
`ASICamera2.dll` loaded. NINA's profile timeout returned to 60 seconds.

This is a real twenty-minute integration on the capped camera near room
temperature, followed by USB readout, factory correction and display through
the plugin. The image remains in NINA's pane. It establishes 1,200-second
operation on this unit; 2,000 seconds is range-checked and simulator-tested but
has not yet been run for its full duration on hardware. CI passed 51 core tests,
12 NINA tests, 16 Rust tests, formatting, Clippy and the release-package checks.
