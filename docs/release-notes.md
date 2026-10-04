# PulsarFab regain 0.5.5.0

This patch adds experimental, explicit ASI662MC Direct USB video acquisition to
the isolated camera worker. SDK and retained still capture remain the defaults;
existing NINA, ASCOM and Alpaca clients do not switch automatically.

- RAW16/bin-1 full frames and ROI, with video exposures from 32 microseconds to
  30 seconds. Other Direct USB models retain their existing still-capture path.
- Reuse sensor setup and calibration between video frames. Exposure/gain/ROI
  changes safely restart the stream; changing the FPS cap alone does not.
- Cap actual grabs at the requested rate, including 0.5 FPS and rates as low as
  0.01 FPS. No catch-up bursts; exposure and readout can reduce actual throughput.
- Cancel exposure and FPS waits, stop the stream on invalid frame sequences or
  transfer failure, and preserve the existing bounded worker isolation.
- Never replay live video as a retained still frame. Shared software WB/AWB
  remains available. No new camera discovery sweep or physical USB reset.

Operator-authorized Windows testing passed the same 21-video-frame matrix on
USB 2 and USB 3, including full frames/ROI, fractional FPS, slow consumers,
exposure changes through 30 seconds, cancellation, restart and return to still
capture. Long-exposure cancellation took approximately 203–218 ms. SDK reference
traces corroborated transport setup. Automated tests use simulations only.

**Limits:** FPS limits grabs, not sensor output; slow consumers may receive
buffered frames rather than the newest scene. Optical accuracy, cold startup,
physical-disconnect recovery, long-running stability and Linux/macOS video
hardware remain unvalidated. This release does not add ASI676MC video support.
No private images, camera serials, raw traces or calibration payloads are bundled.

See [ASI662MC video semantics and evidence](https://github.com/pulsarfab/regain/blob/v0.5.5.0/docs/asi662mc.md).

For Windows ASCOM and the bundled Alpaca server, use
`Regain-ASCOM-0.5.5.0-win-x64-setup.exe`. Install NINA through
`https://nina-plugins.pulsarfab.com/` or `https://nina-plugins.psf-guard.com/`,
or use `Regain-0.5.5.0.zip` manually. The registry retains the existing plugin
identity and minimum NINA version. Rust workspace version is 0.5.5.

Windows release programs are signed by StackFoundry LLC. Regain is Apache-2.0;
`regain-zwo` also carries a ZWO MIT notice, and bundled dependencies retain their
licenses. Existing published assets are unchanged.
