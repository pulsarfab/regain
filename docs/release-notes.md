# PulsarFab regain 0.5.2.0

Control the **Pegasus Astro Falcon Rotator V2** directly over USB serial,
without the vendor SDK or Unity. The same Rust driver powers native NINA,
native Windows ASCOM and the universal Alpaca server.

- **Native NINA and ASCOM:** choose PulsarFab regain Pegasus Falcon V2.
  The shared rotator dialog offers motion, sync, reverse and reference controls.
  ASCOM clients share one serial connection; disconnecting one leaves the others
  connected. FocusCube3 and Falcon now use `Regain.Pegasus.ASCOM.exe`.
- **Alpaca:** add Falcon or CAA instances at `/setup/rotators`. Each receives a
  stable device number and UUID. Existing CAA profiles retain their identity.
- **Rust:** Falcon support lives in `regain-pegasus::falcon`, sharing serial
  framing with FocusCube3. The common executable exposes
  `regain-device pegasus falcon` commands.
- **Origin and multi-turn control:** reset or relabel the mechanical origin,
  or explicitly travel up to +/-450 degrees. Multi-turn movement bypasses normal
  cable-wrap protection; check clearance and cable slack before using it.
  Uncertain writes are never replayed, and motion faults attempt a halt before
  requiring reconnection.

Hardware tests passed on Falcon V2 revision A, firmware 1.8, on Windows:
short moves, crossing zero, +/-450 degree travel, halt during motion, reverse,
origin reset, repeated reconnects, native NINA and shared 32/64-bit ASCOM clients.
Linux/macOS hardware validation remains pending. Falcon V1 is not supported.
The [Falcon guide](https://github.com/pulsarfab/regain/blob/v0.5.2.0/docs/falcon-v2.md)
includes protocol evidence and a screenshot connected to physical hardware.

For Windows ASCOM and the bundled Alpaca server, use
`Regain-ASCOM-0.5.2.0-win-x64-setup.exe`. Install NINA through
`https://nina-plugins.pulsarfab.com/`, or use `Regain-0.5.2.0.zip` manually.
For Cargo installations, update both `regain-alpaca` and `regain-device` to
0.5.2. The CameraKit ZIP is a separate diagnostic tool.

[Documentation and supported hardware](https://pulsarfab.com/docs/regain/) ·
[Install and upgrade](https://pulsarfab.com/docs/regain/install.html)

Windows release binaries are signed by StackFoundry LLC. Regain is Apache-2.0;
`regain-zwo` also carries a ZWO MIT notice, and bundled third-party components
retain their own licenses.
