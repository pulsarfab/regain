# NINA end-to-end acceptance matrix

Status: **attached-hardware acceptance matrix passed**, 2026-09-14. All 11
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
| Retry cutoff | SDK exposure above 30 s with deliberate host termination must fail after one attempt with the default cutoff. Direct without fallback keeps its verified 30 s main/676 or 10 s guide maximum. With explicit fallback, a longer request switches to SDK before exposure. |
| Duo cooling | Record initial temperature/target/enable/power. Exercise a modest target change and enabled/disabled states; capture during cooling. During a short-exposure worker failure, verify target/enable restoration and settling near the prior temperature before retry. Restore initial state. |
| Unsupported direct cameras | Choosing an unverified model such as ASI6200MM Pro P25 with the direct option must be rejected clearly. Duo main and guide now have dedicated direct paths. Confirm the SDK remains usable afterward. |

Do not infer cable-reconnect retention or natural USB-error behavior from a
process-kill test. Physical detach/power-cycle experiments are separate cases.
NINA end-to-end success requires an observed new image in its image pane; the
earlier command-line and simulator results remain separately documented in
[validation](validation.md).

## Final restored state

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
