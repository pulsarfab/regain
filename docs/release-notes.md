# PulsarFab regain 0.5.1.0

The Rust drivers, workers, and Alpaca server are now on crates.io. This release
does not change device behavior.

- **Install the Alpaca server with Cargo:** on Windows, Linux, or macOS, run
  `cargo install regain-alpaca regain-device`, then `regain-alpaca --port 11111`.
  Install both crates to the same directory; the server starts its workers from
  there. ZWO SDK camera mode also needs the SDK library in that directory.
- **Use the drivers in your own Rust code:** `regain-zwo`, `regain-pegasus`,
  `regain-deepskydad`, and `regain-wanderer` control ZWO cameras and accessories,
  FocusCube3, OFP2, and ETA without vendor SDKs. `regain-transport`,
  `regain-worker`, and `regain-core` hold the shared serial, IPC, and camera
  recovery code. Rust 1.89 or later is required.

The NINA plugin, ASCOM drivers, and CameraKit match 0.5.0.0 apart from the
version number. See the
[0.5.0.0 notes](https://github.com/pulsarfab/regain/releases/tag/v0.5.0.0) for
the last feature changes.

For Windows ASCOM and the bundled Alpaca server, use
`Regain-ASCOM-0.5.1.0-win-x64-setup.exe`. Install the NINA plugin through
`https://nina-plugins.pulsarfab.com/`, or use `Regain-0.5.1.0.zip` manually.
The CameraKit ZIP is a separate diagnostic tool.

[Documentation and supported hardware](https://pulsarfab.com/docs/regain/) ·
[Install and upgrade](https://pulsarfab.com/docs/regain/install.html) ·
[Crates](https://crates.io/search?q=regain-)

Windows release binaries are signed by StackFoundry LLC. Regain is Apache-2.0;
`regain-zwo` also carries a ZWO MIT notice, and bundled third-party components
retain their own licenses.
