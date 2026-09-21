# ASCOM cameras

PulsarFab regain has a Rust Alpaca server for Windows, Linux, and macOS, plus a Windows
native COM driver with four camera entries. Both use the same Rust recovery code as
the NINA plugin. The SDK is the default; direct USB and SDK fallback are options
for each camera.

The Windows frontends are included in release 0.3.0.0. Linux and macOS builds
are available as CI artifacts. They have
passed simulated capture tests, including 32-bit and 64-bit COM clients, and
real Windows Alpaca and COM captures listed below. A full ASCOM ConformU run
is still needed. Linux and macOS USB transfers also need hardware testing.

PulsarFab regain is independent software and is not affiliated with or supported by ZWO.

## Run Alpaca

On Windows, extract the `Regain-ASCOM-...-win-x64.zip` release asset. On Linux or
macOS, extract the matching `regain-rust-*` artifact. Keep the server, workers,
and SDK library together. The server and workers need no .NET installation.

```sh
./regain-alpaca --port 11111
```

On Windows use `.\regain-alpaca.exe --port 11111`. Open
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
| `--workers PATH` | Directory containing the Rust worker executables |
| `--sdk PATH` | SDK DLL, SO, or dylib |
| `--simulate` | Use simulated devices without accessing USB |

Default settings are `%LOCALAPPDATA%\Regain\Alpaca\cameras.json` on Windows
and `$XDG_CONFIG_HOME/Regain/Alpaca/cameras.json` on Unix, falling back to
`$HOME/.config/Regain/Alpaca/cameras.json`. The log lives beside the settings
and is also visible on the setup page. Keep the settings file when upgrading.

## Windows COM frontend

Requires Windows x64, .NET Framework 4.8, the ASCOM Platform, and the ZWO Windows
driver for a locally connected camera.

Run `Regain-ASCOM-<version>-win-x64-setup.exe` from the release.
Setup requests administrator access, checks .NET and ASCOM
Platform, and installs all four camera entries for 32-bit and 64-bit clients.
The ZWO USB driver is installed separately. Use **PulsarFab regain ASCOM → Camera setup**
in the Start menu, or the ASCOM Chooser setup button, to select a local camera.
The WPF dialog shares the CAA setup theme, uses the host theme in NINA, and saves
settings automatically. Device, Recovery, Cooler, Timeouts and Controls tabs
configure the camera directly. A setup-only connection closes with the dialog.

Run a newer installer to upgrade in place. Close camera applications and stop
the PulsarFab regain Alpaca server first: setup refuses to replace files while they are
in use. It does not stop an exposure automatically. Remove the package through
Windows **Installed apps**. Upgrades and uninstall keep camera settings and
logs in `%LOCALAPPDATA%\Regain`. No service or firewall rule is installed.
Release installers and uninstallers are signed; ordinary CI builds are unsigned.

For a portable/manual install, extract the complete ASCOM ZIP to a permanent folder. From an
administrator PowerShell in that folder, register it:

```powershell
$p = Start-Process .\Regain.ASCOM.Register.exe -ArgumentList /regserver -Wait -PassThru
if ($p.ExitCode) { throw 'Registration failed; check the ASCOM registration log' }
```

The Chooser entries are **PulsarFab regain Retryable Camera 1** through **4**. Each owns
its local `regain-camera` Rust supervisor over a private pipe, just as the CAA
ASCOM driver owns its HID worker. No HTTP server, IP address, port, browser, or
Alpaca client library is involved. Closing the ASCOM object closes the private
worker; a Windows job also cleans it up if the client crashes.

Choose a camera in setup. With exactly one available camera and no saved choice,
Connect selects it automatically. The serial is saved on connection; use distinct
serials for identical models. Close other controllers before connecting.

Native settings are separate from Alpaca: `%LOCALAPPDATA%\Regain\ASCOM\camera-1.json`
through `camera-4.json`. `REGAIN_ASCOM_PROFILES` can select another directory.
Existing `server.json` settings are no longer used; select each local camera
once in the new setup dialog. Alpaca profiles remain available for network use.
For remote cameras, use the ASCOM Platform's Alpaca Chooser/discovery support.

To remove the native entries, run the registration command with `/unregserver`
before removing a portable install's folder.

### Build the Windows installer

After `scripts/build.ps1` and `scripts/build-ascom.ps1`, run
`scripts/install-inno.ps1` once to install the pinned Inno Setup compiler under
`artifacts/tools`, then run `scripts/build-ascom-installer.ps1`. An existing
Inno Setup installation can be selected with `-Compiler <path-to-ISCC.exe>`.
The output is an EXE and SHA-256 file in `artifacts`.

CI runs `scripts/test-ascom-installer.ps1` on a disposable administrator runner.
It checks the missing-platform gate, rollback after a denied registry write,
installation, all four COM slots in both client architectures, capture and abort,
busy-file guards, repair, downgrade and directory-change rejection, uninstall
and settings preservation. The ASCOM
Platform registry version is a fixture; the captures use the installed private Rust
worker in simulation mode. Never run this test on a workstation with installed
camera registrations.

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
through `Regain.Controls`, `Regain.SetControl`, and `Regain.Diagnostics` actions.

ROI width must be a multiple of 8 and height a multiple of 2 in binned pixels.
The direct driver also has camera-specific minimum sizes and origin alignment,
shown in setup. An incompatible ROI is rejected at StartExposure. Some full
binned sensor sizes are not aligned: for example, use NumX 4784 rather than 4788
for ASI6200 bin 2. Apps that always request the unaligned full width need a
compatible ROI. PulsarFab regain does not pad images with invented edge pixels.

StopExposure, asymmetric binning, pulse guiding, fast readout, live view, and
trigger modes are not implemented. Capability properties report this. Abort
discards the incomplete image. Temperature and cooler power update during
exposures; pending cooler changes apply afterward. Disconnect every client
before editing setup.

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
These captures also passed through the COM DLL with temporary per-user
registrations. Clean Windows CI checks installation, activation from both client
architectures, and unregistration, including a directory with spaces in its name.

With the P25 cooler running, killing its worker during a five-second exposure
caused one replacement exposure and restored the 10°C target:

| Mode | Prior temperature / output | At cooling recovery | Time until image |
| --- | --- | --- | --- |
| SDK | 25.6°C / 15% | 25.6°C / 6% | 34.3 seconds |
| Direct | 24.6°C / 19% | 23.5°C / 25% | 15.1 seconds |

Both returned an image before reaching the target, as intended. The prior
setpoint and enable setting were restored after testing. This tests worker
failure, not USB removal or loss of camera power.

## CAA over Alpaca

The standalone `regain-alpaca` server also serves **PulsarFab regain CAA Rotator** as
`/api/v1/rotator/0`, implementing IRotatorV3. Open **CAA rotator setup** from the
server setup page, find the CAA, and select its serial. Only a configured rotator
appears in Alpaca discovery. Its UUID remains stable when the selected device
changes. The setup page provides connection, halt, mechanical movement, sync,
and reverse; the additional reference and multi-turn controls use the same
`Regain.CAA.*` actions documented in [CAA setup](caa-frontends.md).

Keep `regain-device` beside the server. The server owns that same SDK-free HID
worker, preserving its motion limits, deadlines and no-retry behavior. The CAA
profile and logical offset are stored beside `--profiles`, replacing the file
extension with `.rotator.json`. Alpaca, native ASCOM and NINA profiles are
independent. Disconnect the native frontend before connecting through Alpaca.
`--simulate` uses a simulated CAA without opening USB.

## Native CAA rotator

The installer also registers one **PulsarFab regain CAA Rotator** entry, implementing
`IRotatorV3` for 32-bit and 64-bit clients. It selects the CAA by saved serial
and uses the local Rust HID worker directly. It does not use the camera Alpaca
server. Setup exposes origin zeroing, reference assignment, the tested 361°
limit, and explicit segmented multi-turn travel. See [CAA setup and actions](caa-frontends.md).

## EFW and EAF

The same installer registers `ASCOM.ZWOgain.FilterWheel` (IFilterWheelV2) and
`ASCOM.ZWOgain.Focuser` (IFocuserV3). They launch `regain-device.exe zwo` directly
and share the CAA setup theme. The Start menu includes both setup dialogs.
See [accessory setup and USB validation](accessories.md) for serial selection,
filter metadata, motor settings, protocol traces, and supported hardware.

## OFP2 cover and flat panel

Current source and CI builds add **PulsarFab regain Deep Sky Dad OFP2**
(`ASCOM.Regain.OFP2.CoverCalibrator`) as a native ASCOM CoverCalibrator.
It uses `Regain.Ofp2.ASCOM.exe` and a shared Rust serial worker: 32-bit and 64-bit
clients hold independent connections, and the last disconnect releases the port.
The matching setup dialog provides cover Open/Close/Halt and brightness 0–4096.
NINA can select this driver through its ASCOM flat-panel chooser.

Release 0.4.0.0 includes Alpaca support only. The Alpaca server continues to expose
OFP2 as CoverCalibrator 0; disconnect it and the vendor driver before using native
ASCOM. See [OFP2 setup and sharing](ofp2.md#native-windows-ascom).

## Pegasus Astro FocusCube3

Choose **PulsarFab regain Pegasus FocusCube3** (`ASCOM.ZWOgain.FocusCube3.Focuser`).
Unlike the in-process camera and ZWO accessory COM classes, this focuser uses
`Regain.FocusCube.ASCOM.exe` as a shared COM local server. Separate 32-bit and
64-bit clients share one Rust serial worker; the final disconnect releases it.
The FocusCube3 Start menu setup uses this server too. Close Unity's device
connection before using it. The native NINA provider and Alpaca still require
their own exclusive port ownership; broader sharing is deferred.

See [FocusCube3 setup and protocol](focuscube3.md) and its physical-device
screenshots. The serial worker is `regain-device.exe pegasus fc3`, not an Alpaca bridge.

## Wanderer Astro ETA M54 (source/CI)

[ETA setup and protocol](eta.md) covers the SDK-free Rust worker, native NINA
provider, shared Windows ASCOM focuser, and Alpaca focuser 2. Back-focus moves
preserve tilt; setup exposes individual points. Select by serial port. No
hardware halt command is documented. Physical identity and encoder reads are
verified; movement validation is pending.
