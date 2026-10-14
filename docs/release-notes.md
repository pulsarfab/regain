# PulsarFab regain 0.6.1.7

Stop failed mount recovery safely, with tracking disabled and no parking. Add
sensor AutoHome through Green Swamp Server's existing server connection.

- **Stop-only failure handling** aborts motion, stops guiding, disables tracking
  and interrupts acquisition after any runtime recovery failure, cancellation or
  deadline. It never parks, re-homes, returns to the target or restarts guiding
  after failure. A late driver command is stopped again before ownership ends.
- **Inspection latch** survives wrapper recreation and NINA restart through the
  durable journal. It keeps tracking off and blocks further motion, including
  park/unpark, pulse guiding and raw/custom command bypasses. Inspection cannot
  clear a pending operation or unconfirmed stopped state.
- **GSS sensor homing** uses `GS.SkyApi` with a configurable search limit and Dec
  offset. It never falls back to coordinate-only FindHome. Cleanup uses the same
  GSS server to stop AutoHome and both axes without another serial connection.
- **Review fixes** stop the mount when a runtime trigger fails validation while
  leaving equipment alone during ordinary settings editing. Pre-existing ASCOM
  home telemetry is rejected because NINA can swallow a failed FindHome call;
  recovery requires a fresh verifiable result.
- Installer-signing dependencies resolve early with bounded retries so a package
  service outage does not first appear after the long build/test stages.

GSS 1.2.2.7 has an unreadable `LastAutoHomeError` getter after homing. Regain now
treats that as a failure and stops; automatic target return is blocked until GSS
provides readable completion status. Disconnected/unresponsive hardware is logged
as unconfirmed and remains fenced. Physical GSS/AM7 homing, live guiding and
observatory safety acceptance remain outstanding. Enable mount recovery only
after validating your equipment. The prior installed-NINA simulator acceptance
and the full Core/NINA regression suites are documented in the setup guide.

See [mount recovery setup and limits](https://github.com/pulsarfab/regain/blob/v0.6.1.7/docs/ascom-mount-recovery.md).
Use `Regain-0.6.1.7.zip` for NINA or
`Regain-ASCOM-0.6.1.7-win-x64-setup.exe` for Windows ASCOM and Alpaca. Mount
recovery is a NINA provider and sequence feature; it adds no native ASCOM mount
Chooser entry or Alpaca telescope endpoint.

This is a managed-plugin revision. Rust crates remain at the already published
`0.6.1`; no new crates.io upload is required. Existing camera, USB recovery,
accessory and Hub features remain included.
