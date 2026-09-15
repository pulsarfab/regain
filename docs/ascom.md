# ASCOM cameras

ZWOgain has a Rust Alpaca server for Windows, Linux, and macOS, plus a Windows
COM frontend with four camera entries. Both use the same Rust recovery code as
the NINA plugin. The SDK is the default; direct USB and SDK fallback are options
for each camera.

These frontends are available in the source tree and CI artifacts. They have
passed simulated capture tests, including 32-bit and 64-bit COM clients. Real
camera tests through ASCOM and a full ASCOM ConformU run are still needed.
Linux and macOS camera transfers also still need hardware testing.

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
$p = Start-Process .\ZwoGain.ASCOM.exe -ArgumentList /regserver -Wait -PassThru
if ($p.ExitCode) { throw 'Registration failed; check the ASCOM server log' }
```

The Chooser entries are **ZWOgain Retryable Camera 1** through **4**, mapped to
Alpaca devices 0 through 3. Their setup dialog selects the server address and
opens that slot's camera settings. It creates the four slots if needed.
Automatic local Rust server startup is enabled by default. You may instead
connect to an already running server, including one on Linux or macOS.

The COM executable is a local server shared by 32-bit and 64-bit apps. It exits
after clients release their camera objects; the Rust Alpaca service stays up.
Registering the Chooser entries needs administrator access. Machine registration
and automatic COM activation need checking on a clean ASCOM installation;
local tests launch the COM server explicitly.

To remove the entries, run the same command with `/unregserver` before removing
the folder. Server settings are in `%LOCALAPPDATA%\ZwoGain\ASCOM\server.json`;
`ZWOGAIN_ASCOM_SETTINGS` can select another file for a manually launched server.

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
