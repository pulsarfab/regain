# Validation record

Initial development validation: 2026-09-13 (America/Los_Angeles).

- Windows x64, ASI SDK 1.41, NINA.Plugin 3.2.0.9001, .NET 8 target.
- Rust frame-bound test, formatting and Clippy with warnings denied.
- Six direct-driver Rust tests validate the packed descriptor ABI,
  error/length handling, malformed USB descriptor chains, frame boundaries,
  sensor timing/settings and envelope pixel replacement. Five Python analysis
  tests cover incomplete transfer runs, replay separation, endian/scaling detection
  and sparse pixel differences; these run in a separate GitHub CI job.
- 22 .NET supervisor/transport tests: download failure, native host crash,
  hung download watchdog, retry exhaustion, cancellation during exposure and
  reconnect delay, next-capture recovery, opt-in same-frame re-download,
  consecutive binary frames, invalid ROI/configuration, permanent SDK errors,
  cooling enabled/off/deadline, and full-size ASI2600/ASI6200 simulated frames.
  Retry-threshold tests cover exactly 30 seconds, one microsecond above it,
  a configured 60-second boundary, disabling retries with zero, disabling
  same-frame re-download for long exposures, and defaults for existing configs.
- Seven NINA contract/selection tests: embedded logo/Apache manifest metadata, equipment export and complete ICamera
  StartExposure → WaitUntilExposureIsReady → DownloadExposure flow, hiding a
  simulated transfer failure and preserving the original image settings.
  Selection tests cover persistence across new instances, learning the serial,
  loading a newly saved camera on connection, rejecting a missing serial without
  fallback, requiring setup before first connection, and preventing an in-flight
  connection from overwriting a newer camera choice.
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
- Direct Rust probe, no SDK: correct ASI676MC descriptors and five successful
  idle bulk-read cancellation/drain cycles, followed by a successful SDK capture.
  Follow-up transfer comparisons identified exact bin-1 SDK image agreement after
  its correction stage, and two controlled cancellation experiments produced two
  byte-identical complete transfer runs from one exposure command. See the
  [direct-driver evidence](direct-driver-experiments.json) and
  [interpretation/limits](transport-investigation.md#follow-up-direct-rust-io-and-image-processing-2026-09-13).

## SDK-free capture validation

The subsequent SDK-free hardware matrix completed 24 captures, including
32 µs–30 s, 64 × 64 through full resolution, a non-packet-aligned ROI, shifted
ROI, gain/offset changes, and the one-second exposure-mode boundary. Five
recovery/replay cases validated whole-frame identity or retained-prefix identity
without another exposure, including interrupted reads at 3/12/24 MiB and half
of a small ROI. Setting read retries to zero surfaced the deliberate interruption;
a new process then captured/replayed successfully. The binary stream's image
lengths and SHA-256 digests were checked in memory. No image files were saved.
See [SDK-free findings and limits](sdk-free-capture.md) and
[machine-readable evidence](sdk-free-capture-evidence.json).

Automated coverage now includes seven Rust tests (six direct-driver ABI,
framing, timing/settings and envelope-processing tests plus the host test),
22 core tests, seven NINA tests and five Python analysis tests: 41 total.

## Interactive NINA integration

Tested the installed NINA 3.2 application using Windows computer use:

- ZWOgain appeared as its own camera-provider group and selected the ASI676MC.
- The persistent **ZWOgain Retryable Camera** entry opens a camera picker in setup.
  Selected the attached ASI676MC, saved, connected successfully, and reopened
  setup to verify the selected model and automatically remembered serial.
  Restarted NINA and verified both its driver selection and the setup camera /
  serial persisted. Discovery also completed while the camera was connected.
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

USB cable reattachment and natural SDK transfer failures have not yet been
tested. The later [ASI6200MM Pro P25 run](asi6200-p25.md) covers that specific
2025 mono interface. Later desktop acceptance verified Duo cooler restoration and
Light/Dark/Bias sequences on both backends; see the completed
[NINA end-to-end matrix](nina-end-to-end.md). The attached ASI2600MM Duo and
its guide sensor also completed the SDK baseline tests described in
[Linux research / Duo observations](linux-driver-research.md). Simulator tests
exercise supervisory decisions, not vendor firmware behavior.

## Experimental direct backend integration (2026-09-14)

- The SDK remains the default; existing camera settings without a backend field
  load as SDK selections. The SDK-less option is persisted and only accepts the
  verified ASI676MC. A newer setup selection cannot be overwritten by a connection
  that started with a different backend.
- The packaged Rust `--serve` process uses the existing binary protocol, owns the
  camera exclusively, and reports only implemented capabilities. Adapter tests
  verify that a saved SDK descriptor with additional binning modes is replaced
  by the direct backend's bin-1 capabilities on connection.
- All 50 automated tests passed locally: 10 Rust, 26 recovery/protocol, 9 NINA
  contract/selection, and 5 Python analysis tests. Rust formatting and Clippy,
  package/manifest validation, and all five registry-publication fixtures passed.
- The ASI676MC's direct vendor-IN `C8` serial read matched `ASIGetSerialNumber`
  exactly in memory. The direct process then selected and opened that identity
  without loading the SDK. Serial bytes are not published.
- Through the production C# supervisor, three real 3552 × 3552 RAW16 frames
  completed with the direct backend. Deliberately killing the worker immediately
  before the first download caused one reconnect/re-exposure recovery; the next
  two completed with zero recoveries. This is process-failure evidence, not a
  naturally occurring USB transfer failure.
- The package was installed into the local NINA plugin directory. Initial
  Computer Use failures were resolved by restoring the RDP desktop and
  refreshing the connection. All 11 advertised camera/backend/bin combinations
  subsequently passed in NINA's image pane, including the guide sensor and
  direct ASI676MC. ROI, controls, timing, sequences, cancellation and worker
  recovery also passed; see [NINA end-to-end results](nina-end-to-end.md).
- Desktop testing found that the guide SDK clamps requested offsets below 200
  to 200 despite advertising a zero minimum. Implementation `ed3a820` accepts
  SDK offset normalization within the advertised range and preserves the applied
  value in recovery and image metadata. Its expanded .NET suite passed all
  29 core and 10 NINA tests; Rust remained 10 passing tests. The correction was
  installed and verified with guide images in both binning modes and ROIs.

See [factory-correction evidence](factory-correction-evidence.json) for seven
same-frame byte-exact SDK comparisons and the earlier 23-frame direct capture
matrix. Those validate the pixel path separately from the plugin protocol tests.
