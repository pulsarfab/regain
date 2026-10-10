# Continuous focuser temperature compensation

Regain can track temperature with a ZWO EAF or Pegasus FocusCube3 that reports
a valid temperature. The same Rust controller serves native NINA, native
ASCOM, Alpaca and direct Hub sources. Wanderer ETA has no temperature sensor
and does not offer this mode.

Continuous compensation is optional and off by default. It can move the
focuser during exposures: it has no knowledge of a camera controlled by
another application. Use your imaging application's between-exposure
compensation instead when movement during an exposure is unacceptable.
Do not run both compensation loops.

## Configure and enable

1. Choose one backlash owner and use that setting consistently for autofocus,
   calibration and subsequent compensation.
2. Disable `TempComp`. Measure best-focus position at several stable
   temperatures using the same optical configuration and final approach.
   Estimate the signed slope in motor steps per °C; do not assume its sign.
3. In native setup's **Temperature** tab, or the Alpaca focuser setup page,
   allow continuous compensation and enter the slope. Choose a sample interval,
   minimum correction and maximum automatic correction appropriate to your
   setup. Save the settings.
4. Connect and explicitly enable **TempComp**, either through setup or the
   client's focuser interface. The current position and temperature become the
   reference. Tracking is never restored automatically after reconnect.

The target is `referencePosition + stepsPerCelsius × (temperature − referenceTemperature)`.
Regain uses the median of three temperature samples and accumulates changes
until the minimum correction is reached. Targets are calculated from the
fixed reference, avoiding accumulated rounding errors. Defaults are a
30-second interval, a five-step minimum correction, and a 1,000-step maximum
automatic correction. The slope defaults to zero and must be calibrated
before tracking can be enabled.

Disable `TempComp` during autofocus, filter-offset calibration and coefficient
measurement. After autofocus and any filter offset, enable it at the final
focus position. Regain does not infer when an unrelated application is
autofocusing or exposing.

## Backlash

| Owner | Configuration |
| --- | --- |
| Device | Keep **Backlash owner: hardware**. Set backlash in the device; turn off NINA backlash. Regain sends one target. |
| Regain | Choose **regain**, enter the overshoot and final approach direction. Set device backlash to zero and turn off NINA backlash. |
| NINA | Leave continuous Regain compensation disabled, use zero device backlash and the default hardware owner, and let NINA control its moves and backlash. |

With Regain backlash, both explicit and automatic moves first go to an
overshoot position and then approach the requested target in the configured
direction. Both positions must fit inside the travel range; otherwise the
entire request is rejected before movement. `IsMoving` stays true until both
legs finish. Calibrate best focus using this same final approach.

## Interface behavior and faults

The ASCOM/Alpaca `TempComp` property controls tracking. `TempCompAvailable`
is true only after continuous mode and a nonzero coefficient are configured,
with a valid temperature sensor. Otherwise tracking cannot be enabled. An unsupported
setter raises the interface's not-implemented error, including a write of
`false`; reading the unsupported property returns `false`.

An explicit `Move` works with `TempComp` enabled, as required by
[IFocuserV3 and later](https://ascom-standards.org/newdocs/focuser.html).
It takes priority over an automatic correction, finishes at the requested
position, and rebases compensation on the new position and temperature.
Another explicit move cannot take over an unfinished explicit move.
Disabling `TempComp` stops future corrections while an already accepted move
finishes, including its backlash return leg. `Halt` cancels both legs and
disables tracking; re-enable it explicitly to resume.

A missing sensor, external movement, out-of-range target, oversized
correction, motion timeout or failed command acknowledgement suspends
tracking. Regain halts its owned motion where possible and never automatically
replays a move. Inspect the device and explicitly re-enable compensation to
establish a fresh reference. `Regain.Status` and setup show the reference,
pending target, enabled state and last compensation failure.

All clients sharing an ASCOM FocusCube3 server, Alpaca device or Hub source
share one controller and one `TempComp` state. Disconnecting one client does
not disable tracking for another connected client. The last disconnect stops
the worker; the next connection starts with tracking off.

## Shared configuration

The Rust-generated [configuration contract](https://github.com/pulsarfab/regain/blob/main/contracts/focuser-compensation.json)
defines keys, descriptions, defaults and bounds for the native and browser
editors. Native profiles store it under `TemperatureCompensation`; Alpaca
profiles and direct Hub source backends use `temperatureCompensation`:

```json
{
  "continuous": true,
  "stepsPerCelsius": -80,
  "deadbandSteps": 5,
  "intervalSeconds": 30,
  "maxCorrectionSteps": 1000,
  "backlash": "regain",
  "backlashSteps": 50,
  "approach": "increasing"
}
```

The values above illustrate the format; calibrate the slope and overshoot for
your equipment. Saving configuration never enables tracking. Native settings
changes require idle hardware; Alpaca/Hub profile changes require disconnected
clients. Imported ASCOM and Alpaca Hub sources retain their own `TempComp`
implementation; the Hub forwards the property and adds no second loop.

Controller behavior, native NINA, 32/64-bit ASCOM, Alpaca and native Hub paths
are tested with production protocol simulators. These tests do not establish
a useful temperature slope or loaded backlash value for a telescope.
