# ZWOgain

![ZWOgain camera recovery logo](src/ZwoGain.NINA/Assets/zwogain.png)

[![Build and test](https://github.com/theatrus/zwogain/actions/workflows/build.yml/badge.svg)](https://github.com/theatrus/zwogain/actions/workflows/build.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

**ZWOgain**, as in **ZWO Again**, is a N.I.N.A. camera plugin for ZWO ASI cameras.
The camera-and-retry-arrow logo reflects its automatic capture recovery.
The ASI SDK runs in a disposable
Rust process; the NINA adapter keeps the capture request and recovery policy.
An SDK error, failed download, crashed host, or hung command can become a longer
successful capture instead of an interrupted sequence.

**ZWOgain is an independent project and is not affiliated with ZWO in any way.**
It is not endorsed, sponsored, or supported by ZWO.

Initial implementation targeting **Windows x64 and NINA 3.2.0.9001**. Built from
the process isolation, SDK ABI, and plugin packaging patterns in
[AutoPierCam](https://github.com/theatrus/autopiercam).

## Capture recovery

1. Snapshot exposure duration, dark/light flag, RAW16 ROI, symmetric binning,
   gain, offset, USB limit, and supported persistent controls.
2. Expose, poll the SDK's state, and transfer a complete binary RAW16 frame.
3. On a recoverable failure, discard the partial frame and terminate the host.
4. Wait five seconds for USB detach/reattach and SDK state to propagate.
5. Start a fresh host, re-enumerate and reconnect by the original serial number.
6. Restore controls, verify their read-back, restore cooling target and enablement.
7. If cooling was enabled, wait for three consecutive temperature readings within
   2 C of the pre-error reading (or further cooled toward the restored target).
   When power telemetry is available, cooler output
   must also recover to at least its prior level minus 10 percentage points.
   This avoids an early pass while a cold sensor starts warming after the SDK
   resets its regulator. This has a five-minute deadline per attempt.
8. Repeat the original exposure. Only success, cancellation, or final exhaustion
   reaches NINA. The default is three retries after the initial attempt, only
   for exposures of **30 seconds or less**. Longer exposures run normally but
   report failure without taking a replacement exposure. The direct backend
   can first recover a transfer from the retained frame, as described below.

Failed attempts and phases are recorded in NINA's log. The successful image has
the successful attempt's timestamp and requested exposure duration, excluding
recovery time. `ZwoGain.Diagnostics` exposes the latest phase, SDK error code,
SDK exposure state, serial, and SDK version through `ICamera.Action`.

NINA also imposes an outer readiness timeout. During a capture the plugin
temporarily extends that profile value to cover its bounded recovery budget,
then restores it after download, failure, cancellation or disconnect. A user
edit to the timeout takes precedence. The plugin's command, cooling and retry
limits remain in force throughout the wait.

## Experimental SDK-less option

The supervised **ZWO SDK remains the default and primary backend**. In the
camera setup dialog's **Camera** tab, enable **Direct USB driver (experimental)**
to try the separate Rust driver process. Save and reconnect to apply the choice.
The option and **Fall back to SDK** are persisted with the selected camera.
The picker labels the ASI2600MM Pro and ASI220MM Mini guide
separately. **Recovery** contains retry limits and reconnect delay, **Cooling**
contains recovery settling limits, and **Advanced** contains timeouts and frame
read retries. Save and Cancel stay visible on every tab.
Existing settings default to the SDK; fallback is opt-in. With fallback enabled,
a direct open failure or the next permitted exposure retry can switch to the
SDK. Unsupported direct capture settings route to the SDK before exposure.
The selected hardware serial and controls are retained, cooling is restored,
and the SDK stays active until disconnect. Driver info and diagnostics identify
an active fallback. The same retry count and default 30-second retry threshold
apply; fallback never authorizes repeating a longer failed exposure.

The NINA direct backend supports these verified interfaces:

| Camera | RAW16 bins | Direct exposure range | Environment |
| --- | --- | --- | --- |
| ASI676MC USB3 | 1 | 32 µs–30 s | Gain/offset |
| ASI2600MM Pro USB3 | 1–4 | 32 µs–2,000 s | Gain/offset, temperature, cooling, dew heater |
| ASI220MM Mini guide USB2 | 1–2 | Nonzero line integration through 10 s | Gain/offset |

The ASI2600 exposure range is independent of the retry cutoff. A 1,200-second
direct exposure is allowed, but is not automatically repeated after failure
with the default 30-second retry cutoff. The guide and ASI676MC retain their
separate limits. See [NINA hardware tests](docs/nina-end-to-end.md) for tested
durations; the 2,000-second maximum matches the SDK's advertised range.
Full-frame 60- and 1,200-second ASI2600 captures have completed in NINA with
SDK fallback disabled. A full-duration 2,000-second hardware run is not yet tested.

Both Duo sensors are individually selectable in setup. The direct process uses
the installed Windows driver without loading `ASICamera2.dll`, reads hardware
serials and factory defect maps, and applies the verified RAW16 corrections and
software binning. ROIs require at least 64 × 64 physical pixels; main origins
must align to 16 columns and two rows. USB limit is fixed at 40. Main cooling
uses a Rust regulator with a bounded power ramp and the observed nonlinear
current conversion; it runs during idle, exposure and transfer. This controller
is experimental and differs from the SDK's regulator. Unverified models,
including 2600/6200 P25, continue to use the SDK.

ASI676/main read failures can retry the complete retained frame, configurable
from 0 to 5 (default 2), at any supported exposure length. These rereads do not
start another exposure and are independent of the new-exposure retry cutoff.
The guide uses bounded startup stream resynchronization; retained guide-frame
replay has not been established. If transfer recovery fails, the supervisor
reconnects and repeats within the configured policy. Cancellation terminates
the isolated process. Retention across USB removal or power loss is unverified.
See [Duo capture findings](docs/duo-capture.md) for hardware evidence and limits.
See [transfer recovery and SDK gaps](docs/transfer-recovery.md) for cancellation
and timeout tests, the recovery sequence, and work still needed.

See [direct acquisition details](docs/sdk-free-capture.md),
[same-frame correction evidence](docs/factory-defect-correction.md), and the
[procedure for bringing up another camera](docs/camera-bringup.md).

## Build, test, install

Install a current Rust MSVC toolchain, Visual Studio C++ build tools, and the
.NET 8 SDK. ZWO's Windows camera driver must already be installed. The licensed
ASI SDK 1.41 x64 DLL and header are included under `vendor/zwo`.

```powershell
./scripts/test.ps1
./scripts/build.ps1
./scripts/build.ps1 -Install
```

The build creates `artifacts/ZwoGain-0.1.0.0.zip` and its SHA-256 checksum.
Installation copies the package to
`%LOCALAPPDATA%\NINA\Plugins\3.0.0\ZwoGain`. Restart NINA, refresh the camera
chooser, and select **ZWOgain Retryable Camera**. Open its setup gear, pick the camera,
and save before connecting. This entry remains available with no camera attached.
When upgrading from the earlier per-model chooser, select this new entry once.
Disconnect the native
ZWO driver and any other camera application first.

The camera setup dialog provides a camera picker, refresh button, optional SDK
serial number, and recovery settings. The camera choice is saved in
`%LOCALAPPDATA%\ZwoGain\camera.json`; a successful connection remembers its serial
number automatically. The saved camera remains selected across dialog openings
and NINA restarts, including when unplugged. Connection fails if that serial is
missing; it never silently switches cameras. Clear the serial explicitly when
replacing a camera with another of the same model. Selection and recovery changes
take effect on the next connection. Recovery settings remain saved in
`%LOCALAPPDATA%\ZwoGain\recovery.json` and loaded on the next connection. Gain,
offset, USB limit, cooling and dew heater use NINA's usual camera controls.
Defaults for white balance, USB limit, flip, hardware/mono binning, and high-speed
mode follow the native ASI driver; cooling settings already on the camera are
preserved. Settings changed during a capture are applied after that transaction,
so retries continue to use the original settings.

`MaximumRetryExposureSeconds` configures the inclusive duration threshold
(default `30`; `0` disables automatic retries). It applies to both replacement
exposures and the experimental same-frame re-download option. The comparison
uses the requested exposure duration, not elapsed transfer or recovery time.

## Hardware diagnostics

```powershell
cargo build
dotnet run --project src/ZwoGain.Diagnostics
dotnet run --project src/ZwoGain.Diagnostics -- --capture --frames 20 --seconds 0.05
dotnet run --project src/ZwoGain.Diagnostics -- --capture --bin 2 --frames 5
# Deliberately kill the SDK host once after exposure readiness, then recover:
dotnet run --project src/ZwoGain.Diagnostics -- --capture --kill-once --frames 3
# Simulator requires no camera or SDK:
dotnet run --project src/ZwoGain.Diagnostics -- --simulate --capture
```

Use `--camera "ZWO ASI676MC"` when several different models are attached.
`--width`, `--height`, `--x`, and `--y` specify a binned-pixel ROI.
`--host` and `--sdk` select explicit executable/DLL paths. Ctrl+C cancels capture
and terminates the worker. Diagnostic runs do not save image files.

## Current limits

- ASI676MC hardware is tested. ASI2600/6200 frame sizes are tested through the
  simulator; their real cooling, firmware, USB disconnect, and failure behavior
  still need hardware validation.
- Initial selection requires a unique model name or an explicit SDK serial number.
  For multiple cameras of the same model, enter the serial in setup, or first
  connect with only the intended camera of that model attached to remember it.
  Discovery does not open cameras to read their serials. Automatic reconnection
  requires a readable serial number; no index fallback is used.
- The driver supports still RAW16 capture, supported symmetric bins and ROI.
  Live view, asymmetric bins, trigger modes, camera alias editing and native
  ZWO-specific advanced UI are not implemented. Electrons/ADU is unknown.
- Abort/Stop cancels the entire transaction and terminates its host. It does
  not return a truncated exposure. The next capture reconnects and restores state.
- Temperature is cached while the capture transaction owns the camera; cooling
  changes during long exposures are deferred. Normal telemetry/control updates
  run every two seconds when idle.
- There is no documented partial-transfer resume API. Optional
  `ReadyFrameDownloadRetries` is experimental, disabled by default, and retries
  the entire download only while the SDK still reports exposure success.
- No installer signing or NINA registry publication is included in this first
  version. CI and draft-release/registry workflows are provided; see
  [releasing](docs/releasing.md). Local installation, camera contract tests, and interactive NINA 3.2
  camera/capture/recovery checks are covered in the validation record.

See [architecture](docs/architecture.md), [SDK investigation](docs/sdk-lifecycle.md)
and [validation](docs/validation.md). The [transport research and experiment plan](docs/transport-investigation.md)
records direct-driver inspection, SDK-internal replay evidence, and the route
to an independent Rust transport. The experimental `zwogain-direct` executable
now performs complete SDK-free ASI676MC captures with configurable ROI, exposure,
gain and offset, binary RAW16 delivery, and retained-frame readout retries.
Full-frame and interrupted-read replay have been tested without another exposure.
It remains experimental: sensor initialization is model-specific. Independent
factory defect correction matches SDK pixels for ASI676MC and both Duo sensors;
the guide also requires unpacking and low-gain dithering. See [SDK-free capture](docs/sdk-free-capture.md)
for commands, evidence, P25 transport findings and remaining limits.
Licensed under Apache-2.0; bundled vendor
material retains its own license, described in [third-party notices](THIRD_PARTY_NOTICES.md).
