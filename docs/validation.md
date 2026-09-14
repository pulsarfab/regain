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
- Two NINA contract tests: equipment export and complete ICamera
  StartExposure → WaitUntilExposureIsReady → DownloadExposure flow, hiding a
  simulated transfer failure and preserving the original image settings.
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

The actual ASI2600/6200 cameras, cooler restoration, USB cable reattachment,
and natural SDK transfer failures have not yet been tested. Simulator tests
exercise supervisory decisions, not vendor firmware behavior. Interactive
NINA camera chooser/setup/sequencer acceptance remains manual.
