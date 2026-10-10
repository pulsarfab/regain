# PulsarFab regain 0.6.0.0

Regain 0.6 adds the **Regain Hub preview** and improves camera cancellation,
live cooler control and recovery diagnostics. Standalone camera and accessory
integrations remain available alongside the Hub.

## Camera recovery and cooling

- Preserve the camera worker and cooler after an acknowledged capture abort.
  Failed or timed-out stops retire the worker before another capture.
- On explicit disconnect, turn off the cooler and any advertised dew heater,
  with readback in either backend. If the worker is unavailable, attempt bounded
  cleanup of the same serial without restoring its enabled settings.
- Wait for abort cleanup before NINA restarts capture, and distinguish explicit
  client aborts from readiness timeouts. Capture numbers identify start and
  cancellation events in NINA logs.
- Apply cooler target and enable changes during long exposures through the
  shared acknowledged control path. Direct cooling responds immediately to
  enable and setpoint changes, retains steady load through reconnect, and adds
  output for a colder target or a warming sensor. SDK mode retains its vendor
  regulator.
- Report retries across abandoned and successful attempts in recovery logs,
  separately from the delivered frame's retained-read count.
- Preserve measured temperature and cooler power through worker replacement.
  NINA marks held readings and keeps cooling-chart axes intact.
- Limit direct USB read request size independently of the SDK USB Traffic
  percentage. Direct mode can reread retained frames on supported cameras;
  proxying another driver does not add that ability.

The attached ASI585MM Pro passes the real NINA plugin API checks in SDK and
direct mode: live cooler changes, abort/cancel cleanup, preserved worker and
subsequent image downloads. See [physical acceptance](https://github.com/pulsarfab/regain/blob/v0.6.0.0/docs/camera-abort-acceptance.md)
for measurements and test boundaries.

## Regain Hub preview

Configure sources once and publish Camera, Focuser, Rotator, FilterWheel,
CoverCalibrator, Switch, SafetyMonitor and ObservingConditions outputs through
native NINA, Windows ASCOM, Alpaca, or several frontends together.

- Combine switches and gauges, safety monitors, or weather measurements.
- Import supported native Regain devices, Alpaca devices, or installed Windows
  ASCOM drivers through isolated 32-bit and 64-bit helpers.
- Share source connections with explicit ownership for captures and movement.
- Configure coordinated camera captures and focuser groups for NINA sequences.
- Use shared parameter descriptions, validation, discovery, and reviewed
  configuration import/export across native setup and the web editor.

Hub remains a preview. Installed NINA and mixed physical-camera/COM/Alpaca
acceptance evidence is recorded in the repository; environment and conformance
limits remain in [Hub acceptance](https://github.com/pulsarfab/regain/blob/v0.6.0.0/docs/hub-acceptance.md) and [the plan](https://github.com/pulsarfab/regain/blob/v0.6.0.0/docs/hub-plan.md).
An ASCOM-free Windows host and a second LAN host have not been available for
acceptance. Preview status does not waive the release's signing, package or
installer lifecycle checks.

## Installation and support

Use `Regain-0.6.0.0.zip` for NINA or
`Regain-ASCOM-0.6.0.0-win-x64-setup.exe` for Windows ASCOM and Alpaca.
Windows programs and the installer are signed by StackFoundry LLC. Rust crates
use version 0.6.0, including the new `regain-hub` crate. Minimum NINA version and
plugin identity are unchanged.

Direct USB support remains experimental and model-specific. See
[supported hardware](https://github.com/pulsarfab/regain/blob/v0.6.0.0/README.md#supported-hardware),
[capture behavior](https://github.com/pulsarfab/regain/blob/v0.6.0.0/docs/ascom.md#capture-behavior),
[transfer recovery](https://github.com/pulsarfab/regain/blob/v0.6.0.0/docs/transfer-recovery.md) and [Hub setup](https://github.com/pulsarfab/regain/blob/v0.6.0.0/docs/hub-setup.md).
