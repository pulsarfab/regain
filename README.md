# ZWOgain

![ZWOgain logo](src/ZwoGain.NINA/Assets/zwogain.png)

[![Build and test](https://github.com/theatrus/zwogain/actions/workflows/build.yml/badge.svg)](https://github.com/theatrus/zwogain/actions/workflows/build.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

**ZWOgain**, as in **ZWO Again**, is a N.I.N.A. camera plugin that recovers from
ZWO ASI capture errors. It tries to recover the existing frame first, then
reconnects and takes a replacement exposure when the configured policy allows.
NINA keeps the capture request active while recovery runs.

The **ZWO SDK is the default backend**, hosted in a separate Rust process that
can be replaced after a crash or hang. An optional Rust backend captures through
the installed Windows camera driver without loading `ASICamera2.dll`.

Targets **Windows x64 and NINA 3.2.0.9001**. Based on the process isolation and
plugin packaging structure from [AutoPierCam](https://github.com/theatrus/autopiercam).

**ZWOgain is an independent project and is not affiliated with ZWO in any way.**
It is not endorsed, sponsored, or supported by ZWO. ZWOgain code and its logo
are Apache-2.0; bundled vendor material retains its own license. See
[third-party notices](THIRD_PARTY_NOTICES.md).

## Setup

Install the ZWO Windows camera driver and the plugin package while NINA is
closed. Extract the package into `%LOCALAPPDATA%\NINA\Plugins\3.0.0\ZwoGain`,
or use the source build instructions below.

1. Start NINA and select **ZWOgain Retryable Camera** in the camera chooser.
2. Open its setup gear, refresh the camera list, select a camera and save.
3. Connect. Disconnect the native ZWO camera entry and other applications using
   that camera first.

The setup dialog has four tabs:

| Tab | Settings |
| --- | --- |
| Camera | Camera picker, serial, experimental direct driver and SDK fallback |
| Recovery | Full recapture count, exposure cutoff and reconnect delay |
| Cooling | Temperature tolerance, stable readings and settling timeout |
| Advanced | Command/download timeouts, exposure grace and read retry counts |

The main and guide choices are **ASI2600MM Pro** and **ASI220MM Mini (guide)**.
On the tested Pro Duo unit these are separate USB devices; select each normally.
The SDK may report the main device as `ZWO ASI2600MM Duo` in diagnostics.

The camera choice, backend and fallback preference are saved in
`%LOCALAPPDATA%\ZwoGain\camera.json`. A successful connection remembers the
camera's serial. The saved choice remains visible when unplugged. Recovery
settings are saved in `%LOCALAPPDATA%\ZwoGain\recovery.json`. Save and reconnect
to apply setup changes.

For multiple cameras of the same model, enter a serial or initially connect
with only the intended camera of that model attached. A saved serial must
match; the plugin does not silently select another device. Clear it when
replacing a camera. Gain, offset, USB limit, cooling and dew control use NINA's
usual camera controls where supported by the selected backend.

## Recovery policy

**Rereading a frame and taking a new exposure have separate limits.**

| Recovery stage | Default | Exposure cutoff applies? |
| --- | --- | --- |
| SDK reread of a frame still reported ready | 2 retries | No |
| Direct ASI2600/ASI676 retained-frame reread | 2 retries | No |
| Full reconnect, restore and recapture | 3 retries, exposures up to 30 s | Yes |

Read retry counts are configurable from 0 to 5; zero explicitly disables that
read path. Existing saved values are preserved when upgrading, including an
older `ReadyFrameDownloadRetries` value of zero. Set **SDK read retries** to 2
if upgrading from a configuration that disabled SDK rereads.

### Reread first

The SDK path retries the complete download after a retryable SDK error only
while the SDK still reports a ready frame. Its public API has no partial-transfer
resume contract, and many camera-to-SDK errors leave no publicly readable frame.
A hung or crashed worker cannot serve a reread.

The direct ASI2600 and ASI676 paths can restart transfer of the frame retained
in camera memory. They drain outstanding I/O, reset the transfer path and reread
from byte zero without starting another exposure. This is whole-frame replay;
an arbitrary byte-offset continuation has not been established.

These attempts are independent of exposure duration. For example, a failed
transfer after a 1,200-second ASI2600 exposure can be reread even though a new
1,200-second exposure is outside the default recapture limit.

The guide camera's startup resynchronization reads a subsequent sensor frame.
It is therefore subject to the exposure cutoff; same-frame guide replay has
not been established. Retention across unplugging, power loss, device reset
or worker replacement is also unproven.

### Full recapture after rereads fail or are unavailable

When the requested exposure is within the recapture limit, the supervisor:

1. Terminates the failed worker and waits five seconds for USB and SDK state
   changes to propagate.
2. Starts a fresh worker and reconnects to the original camera by serial.
3. Restores the exposure settings and persistent controls, including cooler
   setpoint, enablement and dew heater, and verifies control read-back.
4. If cooling was enabled, waits for three consecutive readings within 2°C of
   the pre-error temperature, or further cooled toward the restored target.
   Where cooler-power telemetry is available, output must also recover to at
   least its previous value minus 10 percentage points. Settling has a default
   five-minute deadline per attempt.
5. Takes a new exposure with the original duration, ROI, binning and controls.

The cooler's **previous setpoint is restored, but recovery waits near the
previous measured temperature**, which need not have reached that setpoint.
Changed controls are deferred until the capture transaction ends, so its retries
continue using the original settings.

`MaximumRetryExposureSeconds` is inclusive and compares the requested exposure
duration, not elapsed recovery time. Its default is `30`; `0` disables full
recapture. It does not disable rereads. If recovery cannot return a complete
frame within the configured budgets, NINA receives the failure.

Recovery phases and errors are recorded in NINA's log. The delivered image uses
the successful exposure's start time and requested duration. `ZwoGain.Diagnostics`
exposes phase, SDK error/state, serial and backend version through `ICamera.Action`.
The plugin temporarily extends NINA's readiness timeout to cover recovery, then
restores it on completion, failure, cancellation or disconnect. A user edit to
that timeout takes precedence.

## Experimental direct backend

Enable **Direct USB driver (experimental)** in setup, save and reconnect. It
uses the installed Windows driver; it does not replace the kernel driver.
The SDK remains the default for new configurations.

| Verified interface | RAW16 bins | Direct exposure range | Controls |
| --- | --- | --- | --- |
| ASI676MC USB3 | 1 | 32 µs–30 s | Gain, offset |
| ASI2600MM Pro main USB3 | 1–4 | 32 µs–2,000 s | Gain, offset, temperature, cooling, dew heater |
| ASI6200MM Pro P25 USB3 (`620b`) | 1–4 | 32 µs–2,000 s | Gain, offset, temperature, cooling, dew heater, fan, LED |
| ASI220MM Mini guide USB2 | 1–2 | Valid nonzero line integration through 10 s | Gain, offset |

The direct backend reads serials and factory calibration, applies the verified
RAW16 defect corrections and software binning, and delivers a complete binary
frame to NINA. ROIs require at least 64 × 64 physical pixels; ASI2600/6200 origins
align to 16 columns and two rows. USB bandwidth is fixed at 40 and bulk reads
are sequential. The main camera's Rust cooling regulator runs during idle,
exposure and transfer; it differs from the SDK regulator.

**Fall back to SDK** is a separate opt-in setting. It permits fallback after a
direct open failure, before exposure for unsupported settings, or on a permitted
full recapture after capture failure. Fallback verifies the same serial and
restores controls and cooling. It remains active until disconnect. It never
bypasses the recapture cutoff to repeat a failed long exposure.

### Hardware validation and remaining limits

- ASI676MC, ASI2600MM Pro main and ASI220MM Mini guide have been captured through
  the plugin and inspected in NINA's image pane. Testing used capped cameras;
  it does not establish illuminated-image performance.
- Full-frame **60- and 1,200-second ASI2600 direct exposures** completed in NINA
  with SDK fallback disabled. The advertised 2,000-second maximum has not yet
  been tested for its full duration on hardware.
- ASI2600 direct tests recovered retained pixels after cancellation at the first,
  middle and last USB chunks, and after a real five-second read deadline.
  Hardware transfer-fault testing extends to 60-second exposures. These were
  controlled faults, not physical disconnects or naturally occurring bus errors.
- Real cooler recovery and direct-to-SDK fallback after worker termination have
  completed in NINA for ASI2600. ASI6200MM Pro P25 now has a model-specific
  direct path, SDK processing comparisons and real transfer-fault tests;
  see the [ASI6200 test coverage](docs/asi6200-p25.md) for its validation status.
  A shared driver package does not prove matching camera protocols.
- The plugin delivers RAW16 still images. Live view, asymmetric binning, trigger
  modes, camera alias editing and the native ZWO advanced UI are not implemented.
  Electrons/ADU is unknown. Direct format/control coverage is narrower than the
  SDK's; see the [SDK gap comparison](docs/transfer-recovery.md).
- NINA temperature/power telemetry is cached during capture; control changes are
  deferred. This does not stop the direct worker's cooling regulator.
- Abort/Stop cancels the transaction and terminates the worker. It does not return
  a shortened exposure. The next capture reconnects and restores state.

Detailed evidence: [NINA tests](docs/nina-end-to-end.md),
[transfer fault tests](docs/transfer-recovery.md),
[main and guide capture/processing](docs/duo-capture.md), and
[ASI676 factory correction](docs/factory-defect-correction.md).

## Build, test and install from source

Requires .NET 8, the Rust MSVC toolchain and Visual Studio C++ build tools.
The ASI SDK 1.41 x64 DLL and header are bundled under `vendor/zwo` with their
license. The ZWO Windows camera driver must be installed for hardware access.

```powershell
./scripts/test.ps1
# Close NINA before installing:
./scripts/build.ps1 -Install
./scripts/test-release.ps1
```

Omit `-Install` to build/package only. The build produces
`artifacts/ZwoGain-<version>.zip`, its NINA manifest, logo and `SHA256SUMS`.
The version comes from `Directory.Build.props` and is checked against Cargo.
Local builds are unsigned. Installation copies the staged package into NINA's
plugin directory; restart NINA to load it.

GitHub [Build and test](https://github.com/theatrus/zwogain/actions/workflows/build.yml)
runs automated recovery, NINA contract, Rust and research tests, builds the
package and validates publication rules. Hardware tests run locally.

The [Release workflow](.github/workflows/release.yml) stages the build, signs
both ZWOgain DLLs, both Rust workers and the camera kit executable with Azure
Trusted Signing, verifies signatures and then packages them. A matching version
tag creates a draft release containing the plugin and standalone kit. Ordinary
pushes do not publish a release or registry entry. Registry publication to
[the NINA plugin feed](https://nina-plugins.psf-guard.com/) requires a published
release with public assets and registry write access. See
[release and registry instructions](docs/releasing.md).

## Diagnostics and protocol research

To contribute a new camera model, use the standalone
[camera exercise kit](scripts/camera-kit/README.md). Download the camera kit ZIP
from [GitHub Releases](https://github.com/theatrus/zwogain/releases),
extract it and run `ZwoGain-CameraKit.exe`. It needs Windows x64 and
the ZWO driver; Python, Rust and NINA are not required. Pick a capped camera to
record initialization, ROI/binning, timing and control sweeps, calibration reads,
and optional matching USB/SDK pixel samples in a local evidence ZIP. Review it
before sharing. Quick and extended sets, cooling exercises and command-line
options are described in the kit README.

Disconnect NINA and other camera applications before hardware diagnostics.

```powershell
cargo build --locked
# List SDK cameras:
dotnet run --project src/ZwoGain.Diagnostics
# Capture using the exact name returned by discovery:
dotnet run --project src/ZwoGain.Diagnostics -- --capture --camera "ZWO ASI676MC" --frames 20 --seconds 0.05
# Direct main-camera capture (the SDK identity includes "Duo"):
dotnet run --project src/ZwoGain.Diagnostics -- --direct --capture --camera "ZWO ASI2600MM Duo" --seconds 60 --frames 1
# No camera required:
dotnet run --project src/ZwoGain.Diagnostics -- --simulate --capture
```

`--bin`, `--width`, `--height`, `--x` and `--y` select binning and a binned-pixel
ROI. `--host` and `--sdk` override executable/DLL paths. `--sdk-fallback` enables
fallback with `--direct`. `--kill-once` deliberately terminates the worker before
download to exercise full recovery; it does not simulate a resumable USB error.
Ctrl+C cancels capture. This diagnostic command reports metadata and statistics,
not image files.

See [inspection commands](scripts/inspection/README.md),
[SDK lifecycle findings](docs/sdk-lifecycle.md),
[transport investigation](docs/transport-investigation.md),
[direct acquisition](docs/sdk-free-capture.md),
[Linux driver research](docs/linux-driver-research.md) and
[camera bring-up procedure](docs/camera-bringup.md).
