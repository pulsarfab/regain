# Unreleased

- Change camera transfer logging while connected in NINA and native ASCOM setup.
  Circular recording is the default: bounded files keep recent metadata outside
  the host log, then emit a diagnostic batch and cleanup activity after a fault.
  Verbose and off modes remain available.
- Adjust the direct cooler's maximum power change while connected, from 1–100
  percentage points per five seconds (default 25). Startup, setpoint changes and
  recovery boosts honor the limit; explicit shutdown remains immediate.

# PulsarFab regain 0.6.0.7

This patch brings ASI6200 direct acquisition timing closer to the vendor SDK
and records command and USB timing in the NINA log. It addresses confirmed
software differences behind slow image delivery and provides evidence for
investigating the reported one-success-then-retries failure.

## ASI6200 acquisition and recovery

- Initialize the selected camera once when opening it. Reuse validated sensor
  setup after healthy captures and clean cancellation; reinitialize after an
  actual initialization, transfer or cleanup fault.
- Match the SDK's exposure-arm order and settle timing. Queue the frame reads
  before integration so USB readout overlaps exposure instead of adding an
  unnecessary delay afterward.
- Preserve cooler and dew-heater settings across sensor setup. Disconnect
  still disables both supported thermal actuators.
- Keep queued USB buffers alive until terminal completion, including aborts
  and failed submissions. Only attempt retained-image recovery after sensor
  integration completed; a failed submission cannot replay an unexposed frame.
- Avoid repeatedly hashing an unchanged retained frame. Cooling recovery also
  recognizes a stable camera already at its target with zero cooler demand.

Delayed worker diagnostics can no longer overwrite the final attempt failure
in the retry status. Retained-read counts and complete failure logs are
preserved during worker retirement.

The changes apply to the shared backend used by NINA, ASCOM and Alpaca.
The SDK remains the comparison baseline; recovery-specific retained reads do
not have an SDK equivalent. See the
[acquisition workup](https://github.com/pulsarfab/regain/blob/v0.6.0.7/docs/asi6200-sdk-parity.md).

## Diagnostics available in the NINA log

- Record every direct command/USB operation with correlation, requested and
  actual lengths, status and timing. Image buffers are never logged.
- Record every Regain-called SDK API, including status polls and scalar
  results. On Windows, instrument the bundled SDK's resolved USB imports and
  report the actual coverage. No external tracing runtime is required.
- Preserve complete structured records through the worker, supervisor and
  NINA bridges at INFO level. Keep transfer evidence out of the compact driver
  status line. Record managed IPC and NINA delivery timing separately.
- Include ASI6200 register/state snapshots around arming, readout and failures.
  Metadata logging uses a bounded asynchronous queue; exceptional record loss
  is explicitly reported rather than blocking pending camera transfers.
- Set `REGAIN_CAMERA_TRANSFER_LOG=0` before launching the host to turn off
  detailed logging and SDK USB hooks while keeping phases and failure/recovery
  summaries. Restart the host after changing it.

SDK USB coverage is limited to the loaded SDK DLL's resolved Windows imports;
helper DLLs and dynamically resolved functions are outside that boundary.
Other platforms retain SDK API timing. See
[diagnostic fields and limits](https://github.com/pulsarfab/regain/blob/v0.6.0.7/docs/camera-timed-diagnostics.md).

Full 5-second RAW16 captures on the attached original ASI6200MM Pro completed
in 6.28–6.33 seconds with logging enabled, close to the instrumented SDK's
6.24–6.44 seconds. Repeated captures in both logging modes, the complete
11-stage cancellation matrix, and direct/SDK NINA API acceptance passed.
The reported remote failure was not reproduced on this different camera.

## Installation and compatibility

Use `Regain-0.6.0.7.zip` for NINA or
`Regain-ASCOM-0.6.0.7-win-x64-setup.exe` for Windows ASCOM and Alpaca.
Windows programs and the installer are signed by StackFoundry LLC. This is a
package revision with rebuilt workers; it does not publish new Rust crates.
Minimum NINA version, plugin identity and settings are unchanged.

Regain Hub remains a preview. See
[Hub setup](https://github.com/pulsarfab/regain/blob/v0.6.0.7/docs/hub-setup.md)
and [Hub acceptance](https://github.com/pulsarfab/regain/blob/v0.6.0.7/docs/hub-acceptance.md).
Direct USB support remains experimental and model-specific; see
[supported hardware](https://github.com/pulsarfab/regain/blob/v0.6.0.7/README.md#supported-hardware).
