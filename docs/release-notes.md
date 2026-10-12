# PulsarFab regain 0.6.0.8

Change camera diagnostics and the direct cooler's power limit while connected,
from either NINA or native ASCOM setup.

- Circular transfer recording is now the default. Bounded rotating files keep
  recent command, USB and SDK API metadata outside the healthy host log. A fault
  emits that history and five seconds of cleanup activity for diagnosis.
- Select verbose or off at runtime. Image contents are never recorded. Replayed
  diagnostic batches do not change retry counters, and ordinary cancelled
  read-ahead buffers do not trigger fault dumps.
- Adjust the direct cooler's maximum power change from 1–100 percentage points
  per five seconds, default 25. Startup, setpoint changes and recovery boosts
  honor the limit; explicit shutdown remains immediate. The vendor SDK retains
  its own cooler control.

Diagnostic files are stored under `%LOCALAPPDATA%\Regain\Diagnostics` on Windows.
See [camera diagnostics](https://github.com/pulsarfab/regain/blob/v0.6.0.8/docs/camera-timed-diagnostics.md)
for file limits, configuration and SDK instrumentation coverage.

Validation included 18 repeated full-frame, bin-1, five-second RAW16 captures
on the attached ASI6200MM Pro across direct/SDK and all three logging modes,
full-frame abort/restart, live settings changes, circular replay and cooler-rate
checks. The complete affected Rust, managed Core, NINA and ASCOM suites passed.

Use `Regain-0.6.0.8.zip` for NINA or
`Regain-ASCOM-0.6.0.8-win-x64-setup.exe` for Windows ASCOM and Alpaca.
This package revision rebuilds the workers and does not publish new Rust crates.
Regain Hub remains a preview; direct USB support remains model-specific. See
[supported hardware](https://github.com/pulsarfab/regain/blob/v0.6.0.8/README.md#supported-hardware).
