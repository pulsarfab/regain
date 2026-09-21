# PulsarFab regain

![PulsarFab regain — Regain control of your equipment.](assets/regain-wordmark.svg)

[![Build and test](https://github.com/pulsarfab/regain/actions/workflows/build.yml/badge.svg)](https://github.com/pulsarfab/regain/actions/workflows/build.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

**Retryable camera capture. SDK-free device drivers. One Alpaca server for your rig.**

PulsarFab regain connects cameras, rotators, filter wheels, focusers, and flat
panels to **NINA**, **native Windows ASCOM**, and **ASCOM Alpaca**. Rust workers
isolate camera SDK failures, recover interrupted downloads, and control supported
USB and serial devices directly. Choose the integration that fits your setup.

[Documentation](https://pulsarfab.com/docs/regain/) ·
[Downloads](https://github.com/pulsarfab/regain/releases/latest) ·
[Install & upgrade](https://pulsarfab.com/docs/regain/install.html) ·
[Supported hardware](#supported-hardware)

Formerly **ZWOgain**. Existing plugin and ASCOM identities are preserved;
settings migrate automatically. See the [upgrade guide](https://pulsarfab.com/docs/regain/install.html#upgrade).

## What do you want to do?

### Keep NINA or Alpaca captures running through recoverable errors

Select **PulsarFab regain Retryable Camera** in NINA, or configure a camera on
the Alpaca server. Both use the same Rust recovery engine as native ASCOM.
Your imaging application waits while regain handles a recoverable failure.

- **Retry the download before repeating the exposure.** Supported direct drivers
  can reread a frame still held in camera memory, including a long exposure.
  The SDK path can retry while the SDK still reports a ready frame.
- **Reconnect and restore the camera.** Regain remembers controls and cooler
  settings, and waits for cooling recovery before a replacement exposure.
- **Contain a stuck SDK.** A supervised worker can be replaced without taking
  down NINA or the Alpaca server.
- **Preserve a completed image.** A cleanup error after download keeps the image
  and reconnects before the next capture. Abort does not start another exposure.

<a id="how-retries-work"></a>

| Default policy | Limit |
| --- | --- |
| Retry the same frame's download | 2 retries, at any exposure length |
| Reconnect and take a replacement exposure | 3 retries, only for exposures ≤30 seconds |
| Delay before reconnecting | 5 seconds |

These limits are configurable; zero disables a retry type. Exhausted recovery
returns an error to the application. Optional Direct USB → SDK fallback still
obeys the replacement-exposure limit.
[Camera setup and recovery](https://pulsarfab.com/docs/regain/cameras.html).

### Harden USB transfers and use the camera without its SDK

Choose **Direct USB (experimental)** for a supported ASI model. The Rust transport
bounds each whole-frame read, drains cancelled transfers before releasing their
buffers, rejects partial frames, and records chunk-level failure details.

On Windows, the ASI2600MM Pro P25 can also reopen its USB handle within the retry
budget, verify the original camera and previously received pixel chunks, and
reread the retained frame without starting another exposure. Automatic USB port
resets and recovery across worker replacement are not part of this path.

The direct driver never loads the camera SDK. Windows still needs the installed
ZWO camera USB driver. The SDK remains the default for broader model coverage;
direct capture and SDK fallback are separate choices.
[Transport behavior and evidence](docs/usb-lifecycle.md) ·
[Direct driver versus SDK](docs/transfer-recovery.md).

### Put the whole rig on a small headless computer

Run **one regain Alpaca server** beside the telescope and connect your imaging
applications over the LAN. That server exposes all supported core equipment:
camera slots, CAA rotator, EFW filter wheel, EAF and FocusCube3 focusers, and an
OFP2 cover/flat panel. Configure them through a browser and discover them from
Alpaca clients, including Windows applications using ASCOM Platform discovery.

The Rust server and its device workers run without .NET, a desktop UI, or vendor
accessory applications. Linux x86-64 and ARM64 builds suit small headless hosts;
Windows and macOS builds are also available. One server manages the rig while
separate workers isolate device access. Linux/macOS physical USB validation is
still incomplete; see [platform requirements](docs/portable-rust.md).

[Headless and LAN setup](https://pulsarfab.com/docs/regain/alpaca.html) includes USB
permissions, service startup, stable device profiles, and discovery. The server
has no authentication: use a trusted LAN, not public internet exposure.

### Connect more than two ASCOM cameras

The native installer registers **Retryable Camera 1–4** for both 32-bit and
64-bit ASCOM clients. Give each entry its own physical camera and saved serial,
backend, and recovery settings. For example, main imaging, guiding, and a second
imaging train can use three distinct entries. Same-model cameras are selected
by serial; a missing saved camera is not silently replaced.

For network use, configure camera slots on the Alpaca server. Device numbers and
UUIDs remain stable when changing a slot's physical camera.
[Multiple-camera setup](https://pulsarfab.com/docs/regain/ascom.html).

### Keep the ASCOM side small

Use the native Windows drivers for local equipment. The camera driver is a thin
.NET Framework COM adapter: capture and recovery run in Rust workers over private
pipes. No HTTP server or vendor control application is required. Matching setup
dialogs provide camera, recovery, cooler, timeout, and accessory controls.

FocusCube3 ASCOM clients share one local COM server and one serial worker across
32-bit and 64-bit applications. Broader sharing between native NINA, ASCOM,
Alpaca, and Pegasus Unity is deferred; those frontends still need exclusive
ownership relative to one another. NINA can select the ASCOM focuser when sharing
with another ASCOM client is needed.
[Native ASCOM setup](https://pulsarfab.com/docs/regain/ascom.html) ·
[FocusCube3 sharing](https://pulsarfab.com/docs/regain/focuscube3.html#sharing).

## Integration points

| Integration | Host | How it connects | Equipment |
| --- | --- | --- | --- |
| **Native NINA plugin** | Windows x64, NINA ≥3.2.0.9001 | Equipment chooser and setup gear; local Rust workers | Cameras, CAA, EFW, EAF, FocusCube3 |
| **Native ASCOM drivers** | Windows x64; 32/64-bit clients | COM interfaces and local Rust workers | Four camera entries, CAA, EFW, EAF, FocusCube3 |
| **Universal Alpaca server** | Windows, Linux, macOS | HTTP device APIs, discovery, browser setup; Rust workers | Camera slots and all supported accessories, including OFP2 |
| **Rust crates and worker CLIs** | Windows, Linux, macOS | USB/HID/serial libraries and worker protocols | Embed device control, inspect protocols, or build another frontend |

The Alpaca server implements these supported devices directly; it is not a
generic proxy for arbitrary installed ASCOM drivers. The OFP2 connects to NINA
and Windows ASCOM applications through **Alpaca discovery**, not a native plugin
or COM driver. Settings are independent between frontends.

## Supported hardware

<a id="supported-cameras"></a>
<a id="caa-rotator"></a>
<a id="efw-filter-wheel-and-eaf-focuser"></a>
<a id="pegasus-astro-focuscube3"></a>
<a id="ofp2-flat-panel"></a>

| Hardware | SDK-free path | Support and controls |
| --- | --- | --- |
| **ZWO ASI2600MM Pro**, original and P25 | Direct USB; SDK also available | RAW16, ROI, bins 1–4, gain/offset, cooling, dew heater, retained-frame rereads |
| **ZWO ASI6200MM Pro**, original and P25 | Direct USB; SDK also available | RAW16, ROI, bins 1–4, gain/offset, cooling, dew heater, retained-frame rereads |
| **ZWO ASI676MC** | Direct USB; SDK also available | RAW16, bin 1, exposures up to 30 s, retained-frame rereads |
| **ZWO ASI220MM Mini** | Direct USB; SDK also available | Guide camera, bins 1–2, exposures up to 10 s; no same-frame reread |
| **Other ZWO ASI cameras** | SDK required | Models supported by the bundled ZWO SDK; direct support is limited to the models above |
| **ZWO CAA** | Rust USB HID | Rotation, sync, reverse, reference/limits; tested CAA-M54 firmware 1.1.1 |
| **ZWO EFW** | Rust USB HID | Slot selection, names, offsets, direction, calibration; tested seven-position EFW-S-0 firmware 3.6.2 |
| **ZWO EAF** | Rust USB HID | Absolute moves, halt, reverse, backlash, travel limit, temperature; tested EAFN firmware 3.8.1 |
| **Pegasus Astro FocusCube3** | Rust USB serial | Absolute moves, halt, temperature, reverse, backlash, speed; tested firmware 1.8.2 |
| **Deep Sky Dad OFP2** | Rust USB serial | Alpaca CoverCalibrator: cover open/close/halt and brightness 0–4096; tested firmware 1.0.14.2 |

**All listed accessory drivers are SDK-free.** On Windows, CAA/EFW/EAF use the
built-in HID driver; FocusCube3/OFP2 use USB Serial Device. Do not replace these
with WinUSB/Zadig. SDK-free camera capture still uses the ZWO Windows USB driver.

ASI2600/6200 direct exposure limits are 2,000 seconds; hardware tests covered
1,200-second dark frames. USB recovery tests used controlled faults, not cable
removal or power loss. Older EAF firmware, EAF Pro/Bluetooth, and dual-disc wheels
are unsupported. Direct live view and trigger modes are not implemented.
A full ASCOM ConformU run remains outstanding.

Device guides: [cameras](https://pulsarfab.com/docs/regain/cameras.html) ·
[CAA, EFW & EAF](https://pulsarfab.com/docs/regain/accessories.html) ·
[FocusCube3](https://pulsarfab.com/docs/regain/focuscube3.html) ·
[OFP2](https://pulsarfab.com/docs/regain/ofp2.html).

## Get started

### Install in NINA

Add `https://nina-plugins.pulsarfab.com/` as a plugin source, install
**PulsarFab regain**, and restart NINA. Select your device, open its setup gear,
and save the physical camera or accessory serial before connecting.
The existing `https://nina-plugins.psf-guard.com/` source serves the same feed.
[NINA walkthrough](https://pulsarfab.com/docs/regain/nina.html).

### Windows ASCOM setup

Download `Regain-ASCOM-<version>-win-x64-setup.exe` from
[Releases](https://github.com/pulsarfab/regain/releases/latest). Requires Windows
10 or later x64, .NET Framework 4.8, and ASCOM Platform; cameras also need the ZWO
Windows camera driver. Close device-control apps, run setup as administrator,
and choose a regain device in the ASCOM Chooser.
[Installation and portable registration](https://pulsarfab.com/docs/regain/ascom.html) ·
[Build the installer](#build-from-source).

### Alpaca setup

Extract the complete Windows ASCOM ZIP and run:

```powershell
.\regain-alpaca.exe --port 11111
```

Open `http://127.0.0.1:11111/setup`, select devices, and save. On Linux/macOS use
the matching `regain-rust-*` CI artifact or a source build, then run
`./regain-alpaca --port 11111`. Keep the server, workers, and any required SDK
library together. For LAN access, add `--listen <host-LAN-IPv4>` and allow the
HTTP port plus UDP 32227 for discovery.
[Full Alpaca setup](https://pulsarfab.com/docs/regain/alpaca.html).

<a id="settings-and-logs"></a>

For profile locations, port ownership, and logs, see
[settings & troubleshooting](https://pulsarfab.com/docs/regain/troubleshooting.html).
Manual upgrades from ZWOgain should remove the old NINA plugin folder before
extracting the new package; keep saved equipment profiles.

## Screenshots

| Retryable camera settings | Native accessory setup |
| --- | --- |
| [![Alpaca camera recovery settings](docs/images/alpaca-recovery.png)](docs/images/alpaca-recovery.png) | [![Native EFW calibration controls](docs/images/native-efw-calibration.png)](docs/images/native-efw-calibration.png) |
| Actual Alpaca UI using simulation. | Actual WPF controls rendered with a simulated wheel. |
| **Native FocusCube3 setup** | **Alpaca FocusCube3 setup** |
| [![Physical FocusCube3 native settings](docs/images/native-fc3.png)](docs/images/native-fc3.png) | [![Physical FocusCube3 Alpaca setup](docs/images/alpaca-fc3.png)](docs/images/alpaca-fc3.png) |
| Physical FocusCube3, firmware 1.8.2. | Physical FocusCube3, firmware 1.8.2. |

Native captures use the standalone ASCOM theme; NINA supplies its own theme.
Device guides include more screenshots and distinguish physical hardware from
simulation. EFW hardware validation included calibration and full slot sweeps.

## Build from source

Windows builds need .NET 8, stable Rust with the MSVC toolchain, and Visual Studio
C++ build tools. From the checkout:

```powershell
./scripts/test.ps1
./scripts/build.ps1                  # NINA ZIP
./scripts/build-ascom.ps1            # Portable ASCOM ZIP and Alpaca server
./scripts/install-inno.ps1           # Once: installer compiler
./scripts/build-ascom-installer.ps1  # Windows setup EXE
```

Outputs are in `artifacts`. Local and ordinary CI builds are unsigned; Windows
release binaries are signed by **StackFoundry LLC**. Linux/macOS server and
workers build with `cargo build --workspace --release --locked`;
[portable build instructions](docs/portable-rust.md) cover SDK libraries and USB
permissions. Native CI artifacts cover x86-64 and ARM64; they are not macOS-notarized.

<a id="help-add-a-camera"></a>

To help add a camera, use the separate [CameraKit](scripts/camera-kit/README.md)
release ZIP to collect local diagnostics and SDK USB traces. No compiler is
needed and nothing uploads automatically.

[Architecture and worker protocols](docs/architecture.md) ·
[Camera bring-up](docs/camera-bringup.md) ·
[Accessory tracing](docs/accessories.md) ·
[Release process](docs/releasing.md).

PulsarFab regain is independent and is not affiliated with or supported by ZWO.
Code and artwork are [Apache-2.0](LICENSE); bundled components retain their own
licenses. See [third-party notices](THIRD_PARTY_NOTICES.md).
