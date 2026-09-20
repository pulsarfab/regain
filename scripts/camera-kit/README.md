# PulsarFab regain camera exercise kit

This kit records the SDK and USB behavior of a ZWO camera so we can implement
and validate an SDK-less backend for that model. It uses the official SDK in
its own Rust process. It never attaches to NINA, replays captured USB writes,
installs a driver, changes firmware or uploads files.

PulsarFab regain is independent and is not affiliated with ZWO.

## Run the kit

1. Download `Regain-CameraKit-<version>-win-x64.zip` from
   [GitHub Releases](https://github.com/pulsarfab/regain/releases).
   Extract it and keep the entire `Regain-CameraKit` folder together.
   Development builds are also available as the **Regain-camera-kit** artifact
   from [GitHub Actions](https://github.com/pulsarfab/regain/actions/workflows/build.yml);
   those require extracting the artifact ZIP and the kit ZIP inside it.
2. Use Windows x64 with the ZWO Windows camera driver installed. No Python,
   Rust, .NET or NINA installation is required. Release kit executables are
   signed by StackFoundry LLC; ordinary CI kit binaries are unsigned.
3. Close NINA and other camera applications. Cap the camera for dark frames;
   power cooled cameras as you normally would. Connect only one device of each
   model. A Pro Duo main and guide appear separately and need separate runs.
4. Double-click `Regain-CameraKit.exe`. Select the camera and enter its edition,
   USB connection (direct/hub, USB2/3), external power and cap/light conditions.
   The quick set is the default. Choose whether to include small image samples.
5. Review the result and share the generated ZIP with the PulsarFab regain maintainer.
   Nothing is sent automatically. Results go into a new timestamped directory
   under `camera-evidence` in the working directory, or your `--output` folder.

The kit changes imaging controls during testing and attempts to restore their
values and automatic flags afterward. It does not change cooler/dew settings
unless `--exercise-cooling` is requested, but SDK initialization itself can
affect camera state. Restoration is reported in the manifest; unplugging or a
driver hang can prevent it. ROI, format and SDK dark-subtraction state are not
restored. Configure the next imaging session normally.

## Exercise sets

**Quick** records initialization, repeated captures, full frame, a 64 × 64 ROI,
moved and far-edge ROIs, advertised bins up to 16, exposures around the one-second
transition, and gain/offset/bandwidth ranges. Most frames use a 512 × 256 ROI.
Each control variant starts from the same baseline. SDK alignment rejections
and clamping are evidence and are recorded, not silently treated as success.

**Extended** adds shorter exposures, more points around timing transitions,
10/30/60-second exposures, candidate gain boundaries and available flip,
hardware-bin, high-speed, mono-bin and pattern-adjust controls. A candidate
boundary is an experiment, not a claim about the new sensor's behavior.

Both sets are RAW16 only and use the advertised exposure/control ranges. Frame
size is capped at 128 MiB. Out-of-range cases are listed as skipped. SDK errors
are recorded per case; an unresponsive host or a trace failure ends the run.
The default total limit is ten minutes, plus at most 45 seconds for cleanup.
Use Ctrl+C to stop; the partial evidence is still packaged. A second interrupt,
closing the console or killing the kit can prevent cleanup and ZIP creation.

`--exercise-cooling` is optional: it sets a target approximately 2°C below the
measured temperature, enables supported cooler/dew controls briefly, reads
telemetry and restores the original settings. This records control transactions;
it does not validate thermal equilibrium or cooler recovery performance.

## Command-line examples

Run these in PowerShell from the extracted kit folder:

```powershell
.\Regain-CameraKit.exe --list
.\Regain-CameraKit.exe --camera "ZWO ASI2600MM Duo" --include-pixels --notes "Pro Duo; capped; USB3 direct; 12V connected"
.\Regain-CameraKit.exe --camera "ZWO ASI220MM Mini" --profile extended --include-pixels
.\Regain-CameraKit.exe --camera "EXACT NAME FROM LIST" --plan-only
.\Regain-CameraKit.exe --camera "EXACT NAME FROM LIST" --profile extended --deadline 1200 --power-history cold-power-start
.\Regain-CameraKit.exe --camera "EXACT NAME FROM LIST" --exercise-cooling
.\Regain-CameraKit.exe --self-test --include-pixels
```

For a new P25 camera, use its exact SDK name and describe the edition in notes.
Run once after a normal imaging session (`--power-history warm`), and separately
after a physical power cycle (`--power-history cold-power-start`). Perform that
power cycle before starting the kit; the kit never does it for you. The final
close/reopen exercise uses the same SDK process and is not a cold-start test.

Exit status is 0 for a completed run with every attempted case passing, 2 for
failures/incomplete evidence, and 130 for cancellation. A nonzero result can
still contain useful evidence. Inspect `completed`, `passed`, each case outcome,
trace errors and restoration; the existence of a ZIP alone is not success.

## What the ZIP contains

- `manifest.json`: camera capabilities, SDK/host/tracer hashes, build commit,
  original/applied controls, requested exposures, frame sizes/digests, timings,
  temperature/power snapshots, case outcomes and restoration results.
- `plan.json`: the generated exercises. Unsupported ranges are in the manifest.
- `events.jsonl`: SDK entry/exit and USB submission/completion events, sequence
  IDs, status codes, control setup packets and bounded control payloads. This
  includes observed calibration reads and standard descriptors where available.
- `sample-index.json`: successful USB chunks collected for optional sample cases.
- `samples/`: with `--include-pixels`, the first SDK RAW16 frame from each
  small-ROI exercise and its USB chunks; full-frame pixels are omitted.
  Individual chunks are not spliced or represented
  as complete frames. Correlate sequence IDs, timestamps and SDK calls to decode
  framing, packing, binning and correction from the same acquisition.
- `SHA256SUMS`: checksums of the run files. The console also prints the ZIP hash.

Device instance paths and the explicit SDK serial field are omitted. **Vendor
control payloads may contain camera identifiers and factory calibration**, and
optional samples contain real pixels. Notes are included verbatim. Review the
ZIP before sharing; do not post it publicly by default. No NINA logs, unrelated
files, account details or hostnames are collected intentionally.

Trace files are bounded at 64 MiB and optional USB sample files at 256 MiB per
run. Hitting a limit marks evidence incomplete. Hashing and instrumentation
affect timing, so these runs are not throughput benchmarks. The passive hooks
observe the known Windows ZWO/Cypress `DeviceIoControl` transport. If another
driver path produces no matching bulk events, the kit reports that gap instead
of claiming a usable USB trace.

SDK discovery probes all attached ASI interfaces. The transport inventory can
therefore include other connected models. USB IDs on submissions and descriptors
identify those interfaces; capture exercises use only the selected camera.

## How the evidence is used

Compare initialization and control transactions across cases to infer register
fields and timing formulas. Use the raw/SDK pairs to identify byte packing and
processing changes. Calibration belongs to its device; do not hard-code an
individual camera's EEPROM contents in a driver. Unknown models use no private,
version-specific SDK memory hooks in this kit.

A successful exercise set is an SDK baseline, not direct-driver support. New
models still require independent capture, same-frame processing validation,
retained-frame recovery experiments and NINA integration tests. This kit does
not inject USB faults, establish replay commands, trace video/RAW8/RGB modes or
prove recovery after physical disconnects.

## Build and licenses

From the source repository on Windows with Python 3.12 and the normal plugin
build dependencies:

```powershell
python -m pip install -r scripts/camera-kit/requirements.txt
./scripts/build.ps1 -StageOnly
python -m unittest discover -s scripts/camera-kit
python scripts/camera-kit/build.py
```

The folder distribution bundles Python, Frida, the SDK and the Rust host using
PyInstaller. The build runs the frozen executable against the simulator before
creating `artifacts/Regain-CameraKit-<version>-win-x64.zip`. Source scripts are
included under `source/`, with dependency licenses under `licenses/`. The
compiled kit does not need a Python installation or network access at runtime.

CI also checks failed downloads, worker crashes, cancellation, control restoration,
partial ZIP creation and a hung-download deadline against real simulator hosts.
The frozen smoke test runs with Python/toolchain directories removed from PATH.

Release builds use `build.py --prepare-only`, sign the prepared executable,
then run `build.py --package <prepared-folder>`. Checksums are generated after
signing. The prepared folder already contains the signed Rust host from the
plugin staging directory.

PulsarFab regain scripts are Apache-2.0. Python, Frida, PyInstaller and the vendor SDK
retain their own licenses; see the bundled notices. Frida's native extension
is a separate file under `runtime/` and can be replaced when rebuilding.
