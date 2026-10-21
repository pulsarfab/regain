# PulsarFab regain 0.6.1.9

Recover Regain EFW and CAA USB connections without losing track of the optics.

- Native NINA and Windows ASCOM EFW/CAA drivers reopen the selected hardware serial after a failed read, with bounded attempts. Recovery verifies the same filter slot or idle rotator position and settings, and preserves shared ASCOM client connections. Uncertain writes and moves are never replayed.
- The NINA retryable camera verifies previously connected Regain optics before exposure and before image delivery. Changed or unavailable optics block imaging until explicit reconnection. A frame spanning an optical disconnect is withheld even when the final position matches; the next fresh capture can proceed after the connection and optical state are verified.
- **Verify optical devices and recover USB connections** controls this NINA integration and is enabled by default. Recovery diagnostics appear in NINA's log and the accessory/rotator logs. Intentional accessory disconnects are respected and cannot silently remove an optical device from verification.

Validation includes the full NINA and core suites, controlled EFW/CAA worker faults, and an installed NINA pass with a physical CAA-M54 and ASI6200MM Pro. Two CAA USB port cycles recovered with unchanged optics; an interrupted frame was rejected and subsequent full-frame RAW16 captures succeeded. Physical EFW reconnection, extended absences and whole-cable power loss remain unvalidated. Spontaneous recovery is scoped to Regain's native NINA and Windows ASCOM accessory drivers; it does not add this behavior to third-party ASCOM drivers or Rust Alpaca accessory workers.

See [USB and optical recovery](https://github.com/pulsarfab/regain/blob/v0.6.1.9/docs/usb-recovery.md).
Use `Regain-0.6.1.9.zip` for NINA or `Regain-ASCOM-0.6.1.9-win-x64-setup.exe` for Windows ASCOM and Alpaca.

This is a managed-plugin revision. Rust crates remain at the already published `0.6.1`; no new crates.io upload is required. Existing camera recovery, USB reset, mount recovery, accessory and Hub features remain included.
