# PulsarFab regain 0.6.1.8

Record which physical camera produced your lights and calibration frames.

- **Optional camera serial metadata**: enable **Save camera serial number in
  image metadata** in the Regain NINA camera setup, then reconnect. Both SDK and
  Direct USB modes write the confirmed hardware serial as the custom `CAMSER`
  string keyword in FITS and XISF images.
- **Off by default**, including older camera selections. Unavailable serials
  are omitted. Leading zeros are preserved, and camera model names and saved
  NINA driver IDs remain unchanged.
- The serial comes from the connected backend, rather than a saved selection,
  USB port or editable alias. No extra USB queries are made during capture or
  download. Changing a saved selection affects the next connection.

Use `CAMSER` to distinguish data from cameras of the same model. It identifies
the camera, not the whole optical train; gain, temperature, filters and other
calibration settings still matter. This setting belongs to the Regain NINA
Retryable Camera plugin; other imaging applications control their own metadata.

The full local NINA suite passed: 666 tests, with five hardware/COM tests skipped.
Regression coverage includes full-frame simulated SDK and Direct USB captures,
FITS/XISF headers, older saved selections and selection changes while connected.
Physical camera acceptance was not repeated for this metadata-only change.

See [camera identity metadata](https://github.com/pulsarfab/regain/blob/v0.6.1.8/docs/nina-camera-fits.md).
Use `Regain-0.6.1.8.zip` for NINA or
`Regain-ASCOM-0.6.1.8-win-x64-setup.exe` for Windows ASCOM and Alpaca.

This is a managed-plugin revision. Rust crates remain at the already published
`0.6.1`; no new crates.io upload is required. Existing camera recovery, USB reset,
mount recovery, accessory and Hub features remain included.
