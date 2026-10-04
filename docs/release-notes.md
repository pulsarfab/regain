# PulsarFab regain 0.5.7.0

This patch hardens experimental ASI662MC and ASI676MC Direct USB video against
a transient malformed frame. SDK and retained still capture remain defaults;
NINA, ASCOM and Alpaca clients do not switch modes automatically.

- Discard one invalid video frame envelope, restart the stream on the same
  camera handle and acquire a fresh validated frame before reporting failure.
- Honor the FPS cap during recovery, preserve cancellation, and advertise the
  bounded recovery allowance to consumers. Never substitute replayed pixels.
- A second malformed frame, partial transfer or unrelated error remains
  visible. No discovery sweeps or physical USB resets are added.
- Add simulated SDK failed-exposure statuses for consumer recovery tests.

Validation: 67 ZWO library tests and strict lint passed. Operator-authorized
ASI662MC USB 2 checks passed the 6.4/25/25/6.4/25-second gain-300 sequence on
both released and candidate workers. Local AutoPierCam checks also passed
seven fixed 60-second frames per backend and an adaptive video-to-still ramp.
The reported USB 2 boundary failure was not reproduced locally; synthetic
faults verify recovery behavior, not the remote system's fault cause.

Optical accuracy, cold startup, physical-disconnect recovery, long-running
stability and Linux/macOS video hardware remain unvalidated. No private images,
camera identities, raw traces or calibration payloads are bundled.

For Windows ASCOM and Alpaca, use `Regain-ASCOM-0.5.7.0-win-x64-setup.exe`.
Install NINA from `https://nina-plugins.pulsarfab.com/` or
`https://nina-plugins.psf-guard.com/`, or use `Regain-0.5.7.0.zip` manually.
Plugin identity and minimum NINA version are unchanged. Rust version is 0.5.7.
Windows release programs are signed by StackFoundry LLC. Existing published
assets are unchanged; dependency licenses remain included.
