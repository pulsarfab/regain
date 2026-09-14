# NINA end-to-end acceptance matrix

Status: **in progress; desktop capture restored**, 2026-09-14. Resetting
the Computer Use JavaScript connection after restoring the desktop resolved the
initial foreground-process error. All ASI676MC and Duo main-sensor SDK binning
modes have now produced new images in NINA's image pane. Remaining cases are
pending, not passes inferred from command-line tests.

Preflight confirmed that the installed `ZwoGain.NINA.dll` matches the local
Release build by SHA-256. GitHub CI passed for implementation `d8491b6` and
documentation commit `c9f5df7`. SDK property enumeration, which does not open
the cameras, confirmed the binning modes below.

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

Completed rows used one second, gain 0, offset 0, USB limit 40, with Save and
Loop disabled. The ASI676MC produced an illuminated scene; the user's cap
description applies to the Duo. No image pixels were saved or committed.
The image-pane dimensions and statistics above were read from screenshots.

For each pending row, select the camera/backend in setup, save, connect, capture a
one-second capped frame and inspect the newly displayed image. Check camera
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
- Setup rejected the saved guide sensor when the experimental option was
  enabled, with an inline instruction to choose ASI676MC or disable that
  option. Selecting ASI676MC then connected successfully using the direct
  backend. The Duo-main rejection still needs a separate visual check.

Entries below are pending full completion unless covered above. Record settings, visible image dimensions,
statistics, capture/recovery outcome and any error text for each completed case.
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
| Retry cutoff | SDK exposure above 30 s with deliberate host termination must fail after one attempt with the default cutoff. Direct must advertise a 30 s maximum and reject longer requests. |
| Duo cooling | Record initial temperature/target/enable/power. Exercise a modest target change and enabled/disabled states; capture during cooling. During a short-exposure worker failure, verify target/enable restoration and settling near the prior temperature before retry. Restore initial state. |
| Unsupported direct cameras | Choosing Duo main or guide with the direct option must be rejected clearly. Confirm the SDK remains usable afterward. |

Do not infer cable-reconnect retention or natural USB-error behavior from a
process-kill test. Physical detach/power-cycle experiments are separate cases.
NINA end-to-end success requires an observed new image in its image pane; the
earlier command-line and simulator results remain separately documented in
[validation](validation.md).

## Issue found during visual testing

NINA's CheckBox theme rendered only ON/OFF and hid the experimental backend
toggle's content. The setup dialog now places its descriptive label in a
separate TextBlock. The correction was installed and visually verified after
restarting NINA. The camera picker selected and persisted the Duo correctly.

The interruption returned `IGraphicsCaptureItemInterop.CreateForMonitor failed:
Could not capture the given monitor. (0x80070057)`. A fresh JavaScript/Computer
Use connection did not resolve this second error. Restoring the RDP desktop
resolved it, and the bin-4 capture subsequently passed.
