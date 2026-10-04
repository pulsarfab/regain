# PulsarFab regain 0.5.8.0

Adds opt-in continuous ZWO acquisition for preview/capture consumers. Existing
NINA, ASCOM and Alpaca single-exposure behavior is unchanged. SDK remains default.

- Drain on the exclusive camera owner independently of IPC and consumer FPS,
  retaining one latest frame instead of accumulating old frames.
- Use native SDK video for SDK cameras. Direct ASI662MC/ASI676MC use native video
  through 30 seconds; other supported Direct families and longer exposures use
  repeated still capture, not an invented native video protocol.
- Apply exposure/gain edits at a drained frame boundary without restarting
  native video. Structural changes still stop/reconfigure/restart. Clear pending
  frames and fence settings transitions before labeling new frames settled.
- Match the observed ASICap long-exposure mode-bit and held timing-write order
  for Direct ASI662MC/ASI676MC. Keep unchanged settings free of per-frame writes.
- Treat SDK video read timeout as a bounded wait with backoff, not an immediate
  exposure fault. Preserve terminal errors, cancellation and existing bounded
  malformed-frame recovery. No additional discovery or implicit USB reset.
- Report settings generations, frame age, acquired/delivered/replaced counts
  and bounded timing diagnostics without image data or camera identities.

Windows operator checks passed full-frame exposure/gain transitions from 234 ms
through 6.4 seconds and back on ASI662MC USB 2 and ASI676MC USB 3, using both SDK
and Direct USB with a 0.5 FPS delivery cap. Automated tests use simulators and
an inert SDK fixture, including slow consumers and blocked output pipes.

The remote hard-lock failure was not reproduced. These changes align acquisition
and control sequencing with observed ASICap behavior; they do not establish a
firmware, interference or power diagnosis. Optical accuracy, cold startup,
physical-disconnect recovery, long-running stability, other camera hardware and
Linux/macOS continuous hardware remain unvalidated. No private images, camera
identities, raw traces or calibration payloads are bundled.

For Windows ASCOM and Alpaca, use `Regain-ASCOM-0.5.8.0-win-x64-setup.exe`.
Install NINA from `https://nina-plugins.pulsarfab.com/` or
`https://nina-plugins.psf-guard.com/`, or use `Regain-0.5.8.0.zip` manually.
Plugin identity and minimum NINA version are unchanged. Rust version is 0.5.8.
Windows release programs are signed by StackFoundry LLC. Existing published
assets are unchanged; dependency licenses remain included.
