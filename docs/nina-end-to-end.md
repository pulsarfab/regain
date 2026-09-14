# NINA end-to-end acceptance matrix

Status: **in progress; desktop capture interrupted**, 2026-09-14. Resetting
the Computer Use JavaScript connection after restoring the desktop resolved the
initial foreground-process error. Three SDK captures have now been observed
in NINA's image pane. A later monitor-capture error interrupted further testing;
remaining cases are pending, not passes inferred from command-line tests.

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
| SDK | ASI676MC | 4 | 888 × 888 | Pending |
| SDK | ASI2600MM Duo | 1 | 6248 × 4176 | Pending |
| SDK | ASI2600MM Duo | 2 | 3120 × 2088 | Pending |
| SDK | ASI2600MM Duo | 3 | 2080 × 1392 | Pending |
| SDK | ASI2600MM Duo | 4 | 1560 × 1044 | Pending |
| SDK | ASI220MM Mini | 1 | 1920 × 1080 | Pending |
| SDK | ASI220MM Mini | 2 | 960 × 540 | Pending |
| Direct | ASI676MC | 1 | 3552 × 3552 | Pending |

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

All entries below are pending full completion. Record settings, visible image dimensions,
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
separate TextBlock. Installing and visually checking that correction requires
closing/restarting NINA after desktop capture becomes available again.

The interruption returned `IGraphicsCaptureItemInterop.CreateForMonitor failed:
Could not capture the given monitor. (0x80070057)`. A fresh JavaScript/Computer
Use connection did not resolve this second error. The bin-4 selection was not
verified and no bin-4 image was counted.
