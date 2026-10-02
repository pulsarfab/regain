# PulsarFab regain 0.5.3.0

Color-camera white balance now has one shared implementation in Regain for
ZWO SDK and Direct USB consumers, including manual gains and automatic white
balance. Existing clients remain unchanged until they opt in.

- **Shared Rust and worker API:** configure Off, Manual, Once, Continuous or
  Locked white balance. Gains are linear red/blue multipliers relative to green,
  not ZWO SDK slider values.
- **Raw by default:** retain unmodified RAW16 pixels with white-balance metadata,
  or explicitly request corrected Bayer pixels. Dark frames are never corrected
  or used for AWB estimates.
- **Bounded AWB:** gray-world estimation rejects dark/clipped samples, holds the
  previous gains when the signal is unsuitable, and caps sampling work. Continuous
  mode smooths changes; Once locks after its first valid estimate.
- **Backend parity:** managed SDK captures neutralize native red/blue balance
  before exposure and verify readback, preventing double correction. Orderly close
  restores the original SDK controls. Direct USB uses the same software engine;
  no USB register programming changes are included.
- **Explicit limits:** managed balance requires a supported color Bayer camera,
  RAW16, binning 1 and no sensor flip. Invalid configurations fail before capture.
  Recovery retains effective gains and mode, including a completed Once estimate.

This release adds the API, not new white-balance controls in NINA/ASCOM/Alpaca
frontends. Tests use simulators and an inert SDK fixture; optical calibration and
physical-camera white-balance behavior have not been validated. Gray-world AWB is
not suitable for every scene, especially narrowband or sparse night-sky imagery.
Forced worker termination cannot guarantee restoration of SDK controls.

See the [white-balance contract](https://github.com/pulsarfab/regain/blob/v0.5.3.0/docs/white-balance.md)
for examples, capabilities and limitations.

For Windows ASCOM and the bundled Alpaca server, use
`Regain-ASCOM-0.5.3.0-win-x64-setup.exe`. Install NINA through
`https://nina-plugins.pulsarfab.com/`, or use `Regain-0.5.3.0.zip` manually.
For Cargo installations, update both `regain-alpaca` and `regain-device` to
0.5.3. The CameraKit ZIP is a separate diagnostic tool.

[Documentation and supported hardware](https://pulsarfab.com/docs/regain/) ·
[Install and upgrade](https://pulsarfab.com/docs/regain/install.html)

Windows release binaries are signed by StackFoundry LLC. Regain is Apache-2.0;
`regain-zwo` also carries a ZWO MIT notice, and bundled third-party components
retain their own licenses.
