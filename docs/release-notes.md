# PulsarFab regain 0.6.1.4

Recover camera connections before an exposure starts, prepare Windows USB resets
before the night, and reconnect affected NINA peripherals before imaging resumes.

- **Automatic USB reset** is an optional shared NINA and native ASCOM setup
  toggle. Approve the selected camera once before imaging; the elevated helper
  survives camera worker, supervisor and application restarts for the Windows
  session. Automatic recovery never opens an elevation prompt. Turning the option
  off immediately removes that camera's approval; the last approval stops the
  helper. Windows Alpaca uses the same helper through the device CLI.
- Helper authentication checks administrator membership and integrity as well as
  Windows' elevation flag, rejecting restricted processes before protocol I/O.
- **Pre-exposure reconnect attempts** bounds connection and settings restoration
  before the first exposure start, including long exposures. This does not permit
  replacing a long exposure or replaying a setting whose acknowledgement was lost.
  Telemetry retains finite last readings while capture owns reconnection.
- **NINA peripheral recovery** observes devices that were connected before a
  camera USB reset, then reconnects affected selections through NINA. Replacement
  captures wait for the observation/reconnection pass and matching reported
  optical state. Regain does not move equipment or restart guiding.
- **Cooler keepalive** refreshes unchanged direct-mode output every five seconds
  after acknowledgement, reducing repeated commands while preserving temperature
  feedback, changed-output updates, recovery restoration and immediate shutdown.
- Improve bounded cancellation cleanup, Windows USB completion diagnostics and
  timeout handling when a helper descendant retains redirected output pipes.
  Installer maintenance detects the running unified device/helper executable.

USB reset remains disabled by default. The Windows operation cycles the selected
camera port; this is not a guaranteed camera-hub or external-power cycle. Camera
hub peripherals may disconnect. Wider companion-port/upstream-hub resets remain
planned work. See [USB recovery setup](https://github.com/pulsarfab/regain/blob/v0.6.1.4/docs/usb-recovery.md).

Validation included complete Rust and managed recovery suites, full-frame direct
and SDK thermal/cancellation captures, and installed NINA acceptance on ASI6200MM
Pro with ZWO CAA ASCOM. The final session-lifetime pass saved nine complete
9576 × 6388, five-second RAW16 bin-1 images with distinct pixel hashes; one physical
camera port cycle completed in 215 ms. The helper survived two NINA exits and a
supervisor restart. The CAA driver reported no disconnect in that pass; a separate
injected-disconnect pass verified its reconnection. Physical Linux USB recovery
and clean-machine/LAN environment checks remain unvalidated locally.

Use `Regain-0.6.1.4.zip` for NINA or
`Regain-ASCOM-0.6.1.4-win-x64-setup.exe` for Windows ASCOM and Alpaca.
Rust workspace crates use version `0.6.1`. Regain Hub remains a preview; direct USB
support remains model-specific. See
[supported hardware](https://github.com/pulsarfab/regain/blob/v0.6.1.4/README.md#supported-hardware).
