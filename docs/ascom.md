# ASCOM cameras

ZWOgain has a Rust Alpaca server for Windows, Linux, and macOS, plus a Windows
COM frontend with four camera entries. Both use the same Rust recovery code as
the NINA plugin. The SDK is the default; direct USB and SDK fallback are options
for each camera.

These frontends are available in the source tree and CI artifacts. They have
passed simulated capture tests, including 32-bit and 64-bit COM clients, and
real Windows Alpaca and COM captures listed below. A full ASCOM ConformU run
is still needed. Linux and macOS USB transfers also need hardware testing.

ZWOgain is independent software and is not affiliated with or supported by ZWO.

## Run Alpaca

On Windows, extract the `ZwoGain-ASCOM-...-win-x64.zip` CI artifact. On Linux or
macOS, extract the matching `zwogain-rust-*` artifact. Keep the server, workers,
and SDK library together. The server and workers need no .NET installation.

```sh
./zwogain-alpaca --port 11111
```

On Windows use `.\zwogain-alpaca.exe --port 11111`. Open
`http://127.0.0.1:11111/setup`, find the cameras, select one, and save. Add slots
for additional cameras. Main and guide cameras are separate USB devices.
Use distinct serial numbers when connecting multiple cameras of the same model.

Choose the device in an Alpaca client. Windows ASCOM clients can also use the
ASCOM Platform's Alpaca discovery/Chooser support. Only configured slots appear
in discovery. Device numbers and UUIDs stay fixed when changing cameras.

The default listener is local only. To use a trusted LAN, select the computer's
IPv4 address with `--listen 192.168.1.10`. Discovery uses UDP 32227 and reports
the HTTP port. This server has no authentication; do not expose it to the public
internet. `--no-discovery` disables UDP discovery.

Run the executable in a terminal, or configure it as an ordinary systemd,
launchd, or Windows startup service. No service is installed automatically.
Use an absolute `--profiles` path for a service and give its account USB access.
See [platform requirements](portable-rust.md) for SDK libraries and USB permissions.

Useful flags:

| Flag | Purpose |
| --- | --- |
| `--profiles PATH` | Camera settings file |
| `--workers PATH` | Directory containing the two worker executables |
| `--sdk PATH` | SDK DLL, SO, or dylib |
| `--simulate` | Use fake cameras without accessing USB |

Default settings are `%LOCALAPPDATA%\ZwoGain\Alpaca\cameras.json` on Windows
and `$XDG_CONFIG_HOME/ZwoGain/Alpaca/cameras.json` on Unix, falling back to
`$HOME/.config/ZwoGain/Alpaca/cameras.json`. The log lives beside the settings
and is also visible on the setup page. Keep the settings file when upgrading.

## Windows COM frontend

Requires Windows x64, .NET Framework 4.8, the ASCOM Platform, and the ZWO Windows
driver. Extract the complete ASCOM package to a permanent folder. From an
administrator PowerShell in that folder, register it:

```powershell
$p = Start-Process .\ZwoGain.ASCOM.Register.exe -ArgumentList /regserver -Wait -PassThru
if ($p.ExitCode) { throw 'Registration failed; check the ASCOM registration log' }
```

The Chooser entries are **ZWOgain Retryable Camera 1** through **4**, mapped to
Alpaca devices 0 through 3. Their setup dialog selects the server address and
opens that slot's camera settings. It creates the four slots if needed.
Automatic local Rust server startup is enabled by default. You may instead
connect to an already running server, including one on Linux or macOS.

The COM DLL runs inside the 32-bit or 64-bit client. Camera access and recovery
stay in the Rust service and its workers. Closing a client leaves the Rust
service running. Registering the Chooser entries needs administrator access.

To remove the entries, run the same command with `/unregserver` before removing
the folder. Server settings are in `%LOCALAPPDATA%\ZwoGain\ASCOM\server.json`;
`ZWOGAIN_ASCOM_SETTINGS` can select another file for a client process.

## Capture behavior

StartExposure returns promptly. The camera stays busy while Rust recovers, and
ImageReady becomes true only for a completed image. Allow enough overall time
in the client for reconnects and cooler recovery. Recovered failures are logged;
an exhausted retry budget returns an ASCOM driver error.

By default, ready-frame downloads get two retries at any exposure length.
Replacement exposures get three retries only at 30 seconds or less. Reconnects
restore the prior setpoint and wait near the prior measured temperature, with
a cooler-output check. Configure these limits on the Recovery and Cooler tabs.

Supported operations include RAW16 images, symmetric binning, ROI, gain,
offset, exposure limits, cooling, and abort. ImageBytes avoids JSON pixel arrays;
JSON ImageArray is also supported. Extra controls and diagnostics are available
through `ZwoGain.Controls`, `ZwoGain.SetControl`, and `ZwoGain.Diagnostics` actions.

ROI width must be a multiple of 8 and height a multiple of 2 in binned pixels.
The direct driver also has camera-specific minimum sizes and origin alignment,
shown in setup. An incompatible ROI is rejected at StartExposure. Some full
binned sensor sizes are not aligned: for example, use NumX 4784 rather than 4788
for ASI6200 bin 2. Apps that always request the unaligned full width need a
compatible ROI. ZWOgain does not pad images with invented edge pixels.

StopExposure, asymmetric binning, pulse guiding, fast readout, live view, and
trigger modes are not implemented. Capability properties report this. Abort
discards the incomplete image. Temperature is cached during capture; pending
cooler changes apply afterward. Disconnect every client before editing setup.

## Build and test

```powershell
./scripts/test.ps1
./scripts/build.ps1 -StageOnly
./scripts/build-ascom.ps1
```

The tests cover shared Rust recovery, HTTP camera operations, persisted slots,
image ordering, setup request validation, and SDK/direct simulated captures
through all four COM slots from both client architectures. No camera is needed.

## Windows hardware checks — 2026-09-15

The standalone Rust server returned 256 × 256 ImageBytes captures at 0.05 seconds:

| Camera | SDK bins | Direct bins |
| --- | --- | --- |
| ASI6200MM Pro P25, capped | 1–4 | 1–4 |
| ASI676MC | 1–4 | 1 |

Abort passed in each mode. These are small-ROI checks, not full-frame ASCOM
validation. The P25 dark-frame means were about 503 ADU in both modes.
All four COM mappings also returned real 64 × 64 images and passed abort from
both 32-bit and 64-bit clients: P25 SDK, ASI676 SDK, ASI676 direct, and P25 direct.
The initial checks used isolated class factories; the DLL loader is also covered
by the simulated tests using temporary per-user registrations.

With the P25 cooler running, killing its worker during a five-second exposure
caused one replacement exposure and restored the 10°C target:

| Mode | Prior temperature / output | At cooling recovery | Time until image |
| --- | --- | --- | --- |
| SDK | 25.6°C / 15% | 25.6°C / 6% | 34.3 seconds |
| Direct | 24.6°C / 19% | 23.5°C / 25% | 15.1 seconds |

Both returned an image before reaching the target, as intended. The prior
setpoint and enable setting were restored after testing. This tests worker
failure, not USB removal or loss of camera power.
