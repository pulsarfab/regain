# PulsarFab regain 0.5.0.0

Recover stalled cameras and connect more equipment through NINA, native ASCOM,
or one Alpaca server.

- **Opt-in USB camera recovery:** NINA, ASCOM, and Alpaca can escalate failed
  captures to a device-scoped reset, reconnect the same serial, restore settings,
  and take a replacement exposure within the configured retry limits. Windows
  uses an elevated hub-port cycle; Linux supports device reset or optional port
  cycling. Disabled by default; this does not switch external 12 V power.
- **Dynamic Alpaca focusers:** add separate EAF, FocusCube3, and ETA slots with
  persistent device numbers and identities, including multiple units of a model.
- **Native OFP2 ASCOM:** the Deep Sky Dad flat panel now has a CoverCalibrator
  entry and setup window. ASCOM clients share one serial connection across
  32-bit and 64-bit applications.
- **Wanderer Astro ETA M54:** direct Rust control, native NINA and shared ASCOM
  support, and Alpaca integration. Physical identity, position reads, and serial
  sharing are verified; physical movement remains unvalidated. M92 is unsupported.
- **Shared vendor crates and executables:** ZWO, Pegasus, DeepSkyDad, and Wanderer
  protocols use vendor crates and common transport code. `regain-device` replaces
  the separate device workers while preserving process isolation per session.
- **Camera hang hardening:** live SDK discovery is rejected while a camera is
  open, and USB timeouts are bounded across supported direct camera models.

USB recovery supports the verified ASI676MC, ASI2600MM Pro/Duo, ASI6200MM Pro,
and ASI220MM Mini identities in SDK or direct mode. A manual Windows ASI676 reset
and fresh RAW16 capture passed. Automatic recovery passed simulation; the physical
automatic test still needs UAC validation. Physical Linux recovery is untested.
See [USB recovery setup and limits](https://github.com/pulsarfab/regain/blob/v0.5.0.0/docs/usb-recovery.md).

For Windows ASCOM and the bundled Alpaca server, use
`Regain-ASCOM-0.5.0.0-win-x64-setup.exe`. Install the NINA plugin through
`https://nina-plugins.pulsarfab.com/`, or use `Regain-0.5.0.0.zip` manually.
The CameraKit ZIP is a separate diagnostic tool. NINA plugin identity and saved
ASCOM identities remain stable. Scripts invoking old per-device executable names
must switch to the unified commands in the
[architecture guide](https://github.com/pulsarfab/regain/blob/v0.5.0.0/docs/architecture.md).

[Documentation and supported hardware](https://pulsarfab.com/docs/regain/) ·
[Install and upgrade](https://pulsarfab.com/docs/regain/install.html)

Windows release binaries are signed by StackFoundry LLC. Linux/macOS server and
worker builds remain available from CI or source. Regain is Apache-2.0; bundled
third-party components retain their own licenses.
