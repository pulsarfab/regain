# PulsarFab regain 0.4.0.0

**Regain control of your equipment.** ZWOgain is now PulsarFab regain, with a
new pulsar-and-return icon, updated setup interfaces, documentation, and package
names. The repository has moved to `pulsarfab/regain`.

- Rust crates and executables use `regain-*`; .NET projects use `Regain.*`.
- The NINA plugin and native ASCOM Chooser entries display PulsarFab regain.
- Saved NINA equipment IDs, the plugin GUID, ASCOM CLSIDs, and ASCOM ProgIDs
  remain stable. Existing settings migrate without overwriting regain profiles.
- The installer recognizes both old and new running binaries and removes obsolete
  files after a successful upgrade. Manual NINA upgrades should remove the old
  plugin folder before extracting the new package.
- Custom actions use `Regain.*`; the old `ZwoGain.*` spelling remains accepted.
- The README includes the new branding and refreshed UI screenshots.

This release also includes the independent Rust OFP2 flat-panel driver with Alpaca
CoverCalibrator support, and Pegasus Astro FocusCube3 support in native NINA,
native ASCOM, and Alpaca. FocusCube3 ASCOM clients share one out-of-process server
and serial connection across 32-bit and 64-bit applications. Broader sharing
between native NINA, Alpaca, and vendor software remains deferred.

Install or update **PulsarFab regain** in NINA using either
`https://nina-plugins.pulsarfab.com/` or `https://nina-plugins.psf-guard.com/`.
Both serve the same registry and preserve the existing ZWOgain plugin identity.

For Windows ASCOM, download `Regain-ASCOM-0.4.0.0-win-x64-setup.exe` below.
The installer includes the standalone Alpaca server. For manual NINA installation,
use `Regain-0.4.0.0.zip`. The CameraKit ZIP is a separate camera diagnostics tool.

See [setup and upgrade instructions](https://github.com/pulsarfab/regain/blob/v0.4.0.0/README.md)
and [compatibility details](https://github.com/pulsarfab/regain/blob/v0.4.0.0/docs/branding.md).

Release binaries use the existing StackFoundry LLC signing certificate. Ordinary
CI builds are unsigned. PulsarFab regain and its artwork use Apache-2.0;
bundled third-party components retain their own licenses.
