# ZWOgain

![ZWOgain logo](src/ZwoGain.NINA/Assets/zwogain.png)

[![Build and test](https://github.com/theatrus/zwogain/actions/workflows/build.yml/badge.svg)](https://github.com/theatrus/zwogain/actions/workflows/build.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

**ZWOgain**, as in **ZWO Again**, is a ZWO ASI camera driver for NINA and ASCOM.
It retries failed downloads and short exposures while the app waits for an image.

The ZWO SDK runs in a separate Rust process, so a camera crash or hang does not
take down NINA. An optional direct driver can capture without the SDK.

The standalone **Rust Alpaca server** runs on Windows, Linux, and macOS without
.NET. It shares recovery with the NINA plugin. Add a saved slot for each camera;
the Windows ASCOM COM frontend provides four fixed slots. See
[ASCOM setup and current limitations](docs/ascom.md). These frontends are new in
the source tree and CI packages; they are not in the v0.2.0.0 release.

**ZWOgain is independent and is not affiliated with or supported by ZWO.**
The code and logo use the Apache-2.0 license. Bundled software has its own
licenses; see [third-party notices](THIRD_PARTY_NOTICES.md).

## Install

Requires **Windows x64**, **NINA 3.2.0.9001 or later**, and the **ZWO Windows
camera driver**.

1. Add `https://nina-plugins.psf-guard.com/` as a plugin source in NINA and install
   **ZWOgain**. Restart NINA.
2. Select **ZWOgain Retryable Camera** in the camera chooser.
3. Open the setup gear, refresh the list, pick your camera, and save.
4. Disconnect other apps using that camera, then connect in NINA.

For manual installation, close NINA and extract the plugin ZIP from
[Releases](https://github.com/theatrus/zwogain/releases/latest) into
`%LOCALAPPDATA%\NINA\Plugins\3.0.0\ZwoGain`.

The camera choice and serial are saved. If you have two cameras of the same
model, enter the serial or connect the intended camera on its own once. Clear
the saved serial when replacing a camera. Save and reconnect after setup changes.

The ASI2600MM Pro main camera and ASI220MM Mini guide camera are separate USB
devices. Both ASI6200 editions appear as **ASI6200MM Pro** in the picker.

## How retries work

| Action | Default |
| --- | --- |
| Retry a download of the same frame | 2 retries, at any exposure length |
| Reconnect and take a new exposure | 3 retries, only for exposures of 30 seconds or less |
| Wait before reconnecting | 5 seconds |

These limits are configurable. A zero count disables that type of retry.

The SDK can retry a download only while it still reports a ready frame. The
direct ASI2600, ASI6200, and ASI676 drivers can reread a frame held in camera
memory. They start the transfer again from byte zero. The guide camera cannot
reread the same frame.

Each direct download attempt uses the configured download timeout (60 seconds
by default). Logs include the failed chunk, completed bytes, and USB status.
On Windows, the direct ASI2600 P25 driver can also reopen its USB handle within
that retry budget. It checks the camera identity and previously downloaded pixels
before returning the recovered frame. [USB lifecycle tests](docs/usb-lifecycle.md)
cover this path; recovery across worker replacement is still experimental.
Windows diagnostic commands can reset or cycle the attached ASI2600 P25's USB
port. Tests required a new exposure afterward, so automatic retries do not use them.

On reconnect, ZWOgain restores the camera settings and cooler setpoint. Before
another exposure, it waits for cooling to return near the temperature measured
before the error. It also checks cooler output; holding the restored setpoint
for 30 seconds is accepted. It need not finish cooling to the setpoint first.

Abort stops the capture. ZWOgain then reconnects to restore controls and cooling
without taking another exposure. A stop/reset error after a successful download
keeps the image and reconnects before the next capture. If recovery fails, NINA
receives the error. Details go in NINA's log.

NINA's normal log includes retry reasons, backend fallback, cooler recovery,
and successful recovery. Recovered failures do not show error dialogs or fail
the capture. Routine frame messages use Debug level.

## Supported cameras

The **SDK is the default**. To try capture without it, enable **Direct USB driver
(experimental)** in setup. The direct driver still needs the installed ZWO
Windows driver.

| Tested camera interface | Binning | Maximum direct exposure |
| --- | --- | --- |
| ASI676MC USB3 | 1 | 30 seconds |
| ASI2600MM Pro (non-P25) main USB3 | 1–4 | 2,000 seconds |
| ASI2600MM Pro P25 USB3 | 1–4 | 2,000 seconds |
| ASI6200MM Pro (non-P25) USB3 | 1–4 | 2,000 seconds |
| ASI6200MM Pro P25 USB3 | 1–4 | 2,000 seconds |
| ASI220MM Mini guide USB2 | 1–2 | 10 seconds |

Other cameras can use SDK mode if supported by the bundled ZWO SDK. ASI2600
P25 and non-P25 ASI6200 support are new in the source tree; they are not in the
v0.2.0.0 release. The ASI6200 editions share a USB ID; the driver reads the
hardware revision to select timing and controls.

Direct capture includes RAW16 images, factory defect correction, gain and
offset. The ASI2600 and ASI6200 also support cooling and the dew heater. The
P25 models have optional fan and LED settings. ASI2600 P25 and original ASI6200
cooler recovery passed in SDK, direct and SDK-fallback modes. Auxiliary controls
were checked against each camera's capabilities.
Selected image areas are adjusted to the camera's size and alignment rules.

**Fall back to SDK** is a separate option. It uses the same camera and stays
active until disconnect. It cannot repeat a failed long exposure unless your
recapture limit allows it.

Testing used dark frames. Full 1,200-second ASI2600 (original and P25) and
ASI6200 P25 captures passed in NINA; the full 2,000-second limit has not been tested.
USB retry tests used controlled faults, not cable removal or power loss.
Live view and trigger modes are not supported. Temperature and cooler power
update during exposures. Control changes wait until capture ends; the direct
cooler controller keeps running.

See [NINA tests](docs/nina-end-to-end.md), [ASI2600 P25 results](docs/asi2600-p25.md),
[ASI6200 P25 results](docs/asi6200-p25.md),
[non-P25 ASI6200 results](docs/asi6200-original.md),
and [transfer recovery and SDK differences](docs/transfer-recovery.md).

## Help add a camera

Download the **Camera Kit** ZIP from
[Releases](https://github.com/theatrus/zwogain/releases/latest), extract it, and
run `ZwoGain-CameraKit.exe`. Close other camera apps and cap the camera first.
No Python or compiler is needed.

The kit tests camera settings and records the SDK's USB traffic in a local ZIP.
Image samples are optional. Nothing uploads automatically; review the files
before sharing. See the [kit instructions](scripts/camera-kit/README.md).

## Build from source

The Rust workers also build on Linux and macOS using native USB access. Camera
testing on those systems is still needed; see [build and test instructions](docs/portable-rust.md).
Native CI packages include SDK 1.41 and standalone camera commands for listing
cameras, inspecting controls, and saving RAW16 captures.
The NINA plugin requires Windows.

Requires .NET 8, the Rust MSVC toolchain, and Visual Studio C++ build tools.
The ASI SDK DLL and header are included.

```powershell
./scripts/test.ps1
# Close NINA before installing:
./scripts/build.ps1 -Install
```

Omit `-Install` to build a ZIP only. Local builds are unsigned. Release binaries
are signed by StackFoundry LLC.

More details: [architecture](docs/architecture.md),
[camera bring-up](docs/camera-bringup.md), [inspection tools](scripts/inspection/README.md),
and [release process](docs/releasing.md).
