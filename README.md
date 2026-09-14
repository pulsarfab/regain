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
   2 C of the pre-error reading. This has a five-minute deadline per attempt.
8. Repeat the original exposure. Only success, cancellation, or final exhaustion
   reaches NINA. The default is three retries after the initial attempt, only
   for exposures of **30 seconds or less**. Longer exposures run normally but
   report their first failure without retrying.

Failed attempts and phases are recorded in NINA's log. The successful image has
the successful attempt's timestamp and requested exposure duration, excluding
recovery time. `ZwoGain.Diagnostics` exposes the latest phase, SDK error code,
SDK exposure state, serial, and SDK version through `ICamera.Action`.

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
chooser, and select **ZWOgain recovery**. Open its setup gear, pick the camera,
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
to an independent Rust transport. Licensed under Apache-2.0; bundled vendor
material retains its own license, described in [third-party notices](THIRD_PARTY_NOTICES.md).
