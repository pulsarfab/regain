# PulsarFab regain 0.5.6.0

This patch extends experimental Direct USB video acquisition to **ASI676MC**,
following the ASI662MC video support in 0.5.5.0. SDK and retained still capture
remain the defaults; NINA, ASCOM and Alpaca clients do not switch automatically.

- Shared video acquisition engine with separate ASI662MC and ASI676MC sensor
  profiles. Model-specific geometry, alignment, gain, offset and timing remain.
- ASI676MC RAW16/bin-1 full frames and ROI, 32 microseconds to 30 seconds.
- Actual-grab FPS cap (0.01–120), cancellable exposure and pacing waits,
  initialization/calibration reuse, and no retained replay of live video.
- Still capture and all other Direct USB models retain their existing paths.
  Shared software WB/AWB remains available. No additional camera discovery or
  physical USB resets are introduced.

Operator-authorized Windows SDK references and Direct USB checks passed on
USB 2 and USB 3. Each Direct matrix covered 21 video frames: full frames/ROI,
fractional FPS, slow consumers, exposure changes through 30 seconds, cancellation,
restart and return to still capture. Long-exposure cancellation took about
218–219 ms. Automated tests use simulations only; pixels were discarded.

**Limits:** FPS caps grabs, not sensor output. Buffered frames may not represent
the newest scene after a slow consumer resumes. Optical accuracy, cold startup,
physical-disconnect recovery, long-running stability and Linux/macOS video
hardware remain unvalidated. No private images, identities, raw traces or
calibration payloads are committed or bundled.

See [ASI676MC video evidence](https://github.com/pulsarfab/regain/blob/v0.5.6.0/docs/asi676-video.md).

For Windows ASCOM and the bundled Alpaca server, use
`Regain-ASCOM-0.5.6.0-win-x64-setup.exe`. Install NINA through
`https://nina-plugins.pulsarfab.com/` or `https://nina-plugins.psf-guard.com/`,
or use `Regain-0.5.6.0.zip` manually. The registry retains the existing plugin
identity and minimum NINA version. Rust workspace version is 0.5.6.

Windows release programs are signed by StackFoundry LLC. Regain is Apache-2.0;
`regain-zwo` also carries a ZWO MIT notice, and bundled dependencies retain their
licenses. Existing published assets are unchanged.
