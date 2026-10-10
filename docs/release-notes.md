# PulsarFab regain 0.6 — camera fixes (unreleased)

- Preserve the camera worker and cooler after an acknowledged capture abort.
  Failed or timed-out stops still retire the worker before another capture.
- Wait for abort cleanup before NINA restarts capture, and distinguish explicit
  client aborts from readiness timeouts. Capture numbers identify start and
  cancellation events in NINA logs.
- Apply cooler target and enable changes during long exposures through the
  shared acknowledged control path. Direct cooling responds immediately to
  enable and setpoint changes, retains steady load through reconnect, and adds
  output for a colder target or a warming sensor. SDK mode retains its vendor
  regulator.
- Report retries across abandoned and successful attempts in recovery logs,
  while keeping the delivered frame's retained-read count separately.

Regression checks use SDK/direct simulated workers, NINA capture/abort/restart
flows and thermal models. These changes have not yet had physical camera
validation. See [capture behavior](ascom.md#capture-behavior) and
[transfer recovery](transfer-recovery.md).

# PulsarFab regain 0.5.10.0

Adds experimental SDK-free support for the **ZWO ASI585MM Pro** in NINA,
native Windows ASCOM, and Alpaca. This is the monochrome cooled camera;
ASI585MC variants are not covered by this driver.

- Capture RAW16 still images at bins 1–4, with ROI, gain, offset, factory defect
  correction, and retained-frame retries after interrupted USB downloads.
- Control cooling through the shared Rust regulator: temperature, target,
  enable, and commanded power. This camera exposes no controllable heater,
  fan-speed, or LED control; unsupported writes are rejected.
- Use explicit worker video and continuous modes through 30-second exposures,
  including live exposure/gain edits and latest-frame delivery. NINA, ASCOM,
  and Alpaca continue to use still captures.
- Service cooling during exposure waits and video delivery delays in the
  shared camera path. ASI585 uses its traced cooler DAC output while larger
  cameras retain their existing output mapping.
- Refresh the README and setup guides for current hardware, USB 2 support,
  optional USB reset, dynamic Alpaca slots, continuous acquisition, and AWB.
  Both Regain camera backends currently capture RAW16.

Windows USB 3 hardware validation included byte-for-byte comparison with the
SDK, bins 1–4, partial-read recovery, 21 video frames through 30 seconds,
continuous settings changes, cooling during a 60-second exposure, and Alpaca
and native camera IPC checks. A **600-second full-frame exposure** recovered
an interrupted download and returned an identical retained image with no
additional exposure. See [ASI585MM Pro support and validation](https://github.com/pulsarfab/regain/blob/v0.5.10.0/docs/asi585mm-pro.md).

Direct support remains experimental. ASI585 USB 2, Linux/macOS hardware,
cold startup, physical USB reset, optical accuracy, and overnight streaming
have not been validated. Other model-specific limits remain documented in
the [README](https://github.com/pulsarfab/regain/blob/v0.5.10.0/README.md).

For Windows ASCOM and Alpaca use `Regain-ASCOM-0.5.10.0-win-x64-setup.exe`.
The NINA plugin package is `Regain-0.5.10.0.zip`, also distributed through the
two public plugin feeds after publication. Minimum NINA version and plugin
identity are unchanged. Rust crates use version 0.5.10. Windows release
programs are signed by StackFoundry LLC.
