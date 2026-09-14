# Validation record

Initial development validation: 2026-09-13 (America/Los_Angeles).

- Windows x64, ASI SDK 1.41, NINA.Plugin 3.2.0.9001, .NET 8 target.
- Rust frame-bound test, formatting and Clippy with warnings denied.
- 22 .NET supervisor/transport tests: download failure, native host crash,
  hung download watchdog, retry exhaustion, cancellation during exposure and
  reconnect delay, next-capture recovery, opt-in same-frame re-download,
  consecutive binary frames, invalid ROI/configuration, permanent SDK errors,
  cooling enabled/off/deadline, and full-size ASI2600/ASI6200 simulated frames.
  Retry-threshold tests cover exactly 30 seconds, one microsecond above it,
  a configured 60-second boundary, disabling retries with zero, disabling
  same-frame re-download for long exposures, and defaults for existing configs.
- Three NINA contract tests: embedded logo/Apache manifest metadata, equipment export and complete ICamera
  StartExposure → WaitUntilExposureIsReady → DownloadExposure flow, hiding a
  simulated transfer failure and preserving the original image settings.
- Release ZIP validation checks required payload/license files and matching
  .NET assembly versions; the generated registry JSON passes NINA's schema.
  Five local publication fixture checks cover a valid package, anonymous-access
  failure, drafts, checksum mismatch and version mismatch without remote writes.
- Attached ZWO ASI676MC: real full-resolution 3552 × 3552 RAW16 captures at
  50 ms exposure. Initial three frames completed in approximately 330–371 ms
  per entire capture/transfer command, with 12,616,704 pixels each.
- Follow-up hardware run: 20 consecutive full-resolution frames at 50 ms,
  all successful (approximately 290–364 ms per capture/transfer).
- Deliberate ASI676MC host termination after readiness: recovered by serial
  after the five-second USB delay, restored controls and returned a fresh frame
  in 6.02 seconds. The next two frames succeeded normally. This is process
  restart validation, not a physical USB reattachment or naturally occurring
  SDK transfer error.
- Hardware ROI run: five successful 512 × 256 RAW16 frames with bin 2 and
  binned origin (16, 8), approximately 169–209 ms per operation.

## Interactive NINA integration

Tested the installed NINA 3.2 application using Windows computer use:

- ZwoGain appeared as its own camera-provider group and selected the ASI676MC.
- Connection displayed 3552 × 3552 geometry, RGGB, 2 um pixels, sensor
  temperature, gain/offset controls, and the USB limit.
- The recovery settings dialog saved a custom 60-second cutoff and displayed
  it on reopening; restored and verified the default 30-second cutoff afterward.
  This exposed and fixed a cross-thread WPF theme-resource crash: NINA launches
  device setup on a separate thread, so the dialog now marshals onto NINA's
  application dispatcher. The dialog also uses NINA's background/foreground
  resources and centers over the main window.
- A one-second full-resolution RAW16 capture displayed successfully in the
  Imaging tab, with 3552 × 3552 dimensions and 16-bit statistics.
- Killed the SDK host during a ten-second exposure. The driver waited five
  seconds, reopened by serial, repeated the ten-second exposure, and NINA
  displayed the replacement image without a failure notification. Total
  capture time was approximately 16.8 seconds before image analysis.
- Killed the host during a 31-second exposure. The log recorded the 30-second
  cutoff and exactly one attempt; NINA surfaced the failure immediately and
  did not start a replacement exposure. NINA's capture and snapshot layers
  each emitted their own error notification for that propagated exception.

The actual ASI2600/6200 cameras, cooler restoration, USB cable reattachment,
and natural SDK transfer failures have not yet been tested. Simulator tests
exercise supervisory decisions, not vendor firmware behavior. Full sequencer
acceptance remains manual.
