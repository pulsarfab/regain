# PulsarFab regain 0.5.12.0

Maintenance release from the 0.5 branch.

- NINA FITS files record the selected camera model in `INSTRUME`, rather than a
  friendly setup label or ASCOM slot name. Native NINA, ASCOM and Alpaca report
  the model consistently; device IDs and custom setup labels remain unchanged.
- Windows Rust workers statically link the MSVC runtime. The NINA ZIP, ASCOM ZIP
  and installer no longer require a separately installed Visual C++
  Redistributable. Existing NINA, .NET, ASCOM and USB driver requirements apply.

Real NINA captures from an attached ASI585MM Pro verified
`INSTRUME = 'ZWO ASI585MM Pro'` in both SDK and Direct USB modes. Regression
checks cover camera metadata, Alpaca discovery and all four ASCOM camera slots
in 32-bit and 64-bit clients. Packaging checks reject VC runtime DLL imports.

After changing the camera model assigned to an Alpaca slot, refresh NINA's
camera chooser and reconnect: NINA caches the discovery name.

Use `Regain-ASCOM-0.5.12.0-win-x64-setup.exe` for the signed Windows installer,
or install `Regain-0.5.12.0.zip` through either public NINA plugin feed.
Minimum NINA version and plugin identity are unchanged. Rust workspace version
is 0.5.12.
