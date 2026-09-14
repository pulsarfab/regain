# Capturing and understanding a camera's SDK initialization

We do not yet understand every ZWO camera's initialization and frame setup.
Treat each sensor/model and transport variant as a separate bring-up target.
A shared Windows driver or similar USB envelope is evidence of reusable
transport plumbing, not proof that sensor registers or recovery commands match.

## Current coverage

| Target | Captured / understood | Still required |
| --- | --- | --- |
| ASI676MC, USB3, RAW16, bin 1 | Volatile initialization; ROI, gain/offset and exposure timing; buffered acquisition; frame envelope; retained-frame replay; active factory correction. Direct captures work and tested correction output matches SDK output byte-for-byte. | Physical cold-power startup, remaining SDK modes/controls, other correction-map classes, production cooling/reconnect integration. |
| ASI2600MM Duo main sensor | Descriptors; SDK initialization/setup and transfer traces; successful 512 × 256 and 6248 × 4176 dark captures; calibration reads observed. | Decode model-specific initialization/timing/control formulas, locate its processing path, implement and validate direct capture, cooling and retention. |
| Duo ASI220MM Mini guide sensor | Separate USB2 interface; SDK initialization/setup and transfer traces; successful 512 × 256 and 1920 × 1080 dark captures; a different frame envelope from the main sensor. | Decode sensor setup, USB2 frame handling and processing; validate direct capture and recovery independently. |
| ASI2600 / ASI6200 Pro P25 variants | Evidence of a shared driver package/interface family. | Actual model/revision descriptors and complete SDK traces; no direct acquisition validation yet. A Duo trace does not establish P25 equivalence. |

The ASI676 implementation remains restricted to the observed model. A trace
records what happened for particular settings; it is not a general camera driver.
Dark frames are sufficient to study transactions, buffer layout and same-frame
pixel correction. They cannot validate Bayer orientation, flips or image geometry
against a scene, or establish illuminated-image performance.

## 1. Identify and record the experiment

Use the installed Windows driver and the repository's owned SDK host. Disconnect
the target camera from NINA and other capture applications. With multiple models
attached, pass the exact SDK name through `--camera-name`. Duplicate names remain
ambiguous and should be tested with only one matching physical camera attached.
The Duo main and guide sensors must have separate trace sets.

Record a local sidecar for each run containing:

- Exact command, source commit and date; expected number of frames.
- SDK version **and DLL SHA-256**, Windows driver version, camera model/revision,
  sensor dimensions, supported bins/formats and USB speed/descriptors.
- Requested exposure, ROI/bin, gain, offset and all other relevant controls;
  distinguish explicitly set values from SDK defaults or previous settings.
- Power/connection history: fresh host process, camera reopen, USB reconnect,
  or physical power cycle. A fresh process does not mean a cold camera.
- Camera cap/light conditions and, for cooled models, cooler state, target,
  sensor temperature and power before/after the run.

Keep serials, device paths, raw traces and local calibration data out of published
evidence. Publish reviewed command parameters, transaction interpretations,
counts, hashes and outcomes. Do not embed per-camera EEPROM payloads in code.

## 2. Capture an SDK baseline

From the repository root, after installing the dependencies described in the
[inspection README](../scripts/inspection/README.md):

```powershell
cargo build --locked
Get-FileHash vendor/zwo/ASICamera2.dll -Algorithm SHA256
# Descriptor-only probe of every interface; no sensor initialization:
target/debug/zwogain-direct.exe --probe-all

# Use a NEW output path on every run. Initial small main-sensor capture:
.reference/inspection-venv/Scripts/python.exe scripts/inspection/trace_transport.py --camera-name 'ZWO ASI2600MM Duo' --width 512 --height 256 --seconds 0.1 --gain 100 --offset 50 --frames 3 --output artifacts/inspection/duo-main-baseline-new.jsonl

# Full main-sensor wire comparison, keeping pixels only in memory:
.reference/inspection-venv/Scripts/python.exe scripts/inspection/trace_transport.py --camera-name 'ZWO ASI2600MM Duo' --width 6248 --height 4176 --seconds 0.1 --gain 100 --offset 50 --compare-wire --output artifacts/inspection/duo-main-full-new.jsonl

# Treat the guide sensor separately:
.reference/inspection-venv/Scripts/python.exe scripts/inspection/trace_transport.py --camera-name 'ZWO ASI220MM Mini' --width 1920 --height 1080 --seconds 0.1 --gain 100 --offset 50 --compare-wire --output artifacts/inspection/duo-guide-full-new.jsonl

.reference/inspection-venv/Scripts/python.exe scripts/inspection/summarize_trace.py artifacts/inspection/duo-main-baseline-new.jsonl
```

`trace_transport.py` starts its own Rust SDK host, attaches Frida before opening
the camera, and records SDK entry/exit and driver I/O. It does not attach to NINA.
The default total deadline is 60 seconds; use `--deadline` up to 300 for a bounded
longer experiment. Exposures are currently limited to 30 seconds. The harness
requests RAW16; other formats and cooler-control sweeps need a harness extension.

Check the complete event stream, not just the exit code: require successful
open/configuration, the requested number of completed frames, valid byte counts
and `experiment-complete`. An exposure failure may be logged with a clean process
exit, particularly in deliberate fault experiments. Do not count it as a pass.
Check reported ROI round trips; the SDK may align or reject requested coordinates.

## 3. Separate lifecycle phases

Build an ordered timeline with SDK-call boundaries, I/O submission/completion
sequence IDs, elapsed times, request direction, request/value/index/length,
response status and returned length. Preserve waits, polls and their conditions.

| Phase | Questions to resolve |
| --- | --- |
| Enumerate/open/init | Which descriptors and identity/calibration reads occur? Which writes initialize volatile sensor/FPGA state? Does initialization depend on an earlier owner or power state? |
| Format/ROI/control setup | Which transactions select bit depth, binning, window origin/dimensions, gain mode, offset and bandwidth? What units, alignment, hold/release groups and byte order apply? |
| Arm/expose | What clears a previous frame? What starts acquisition? How are timing registers calculated? Does long exposure switch to host timing, and at what boundary? |
| Ready/download | Does bulk I/O start before the public SDK says ready? Which status indicates a complete retained frame? What are transfer sizes, timeouts, envelope and sequence semantics? |
| SDK pixel processing | Which bytes are discarded/replaced? Where do defect correction, depth conversion, binning, gamma and flips execute, and in what order? |
| Stop/close/recover | What freezes the sensor without clearing DDR? What clears retention? Which requests must drain before reset? Which controls must be restored after reconnect? |

In particular, do not assume `ASIGetDataAfterExp` initiates the USB download.
The SDK can acquire into internal buffers earlier. Correlate actual driver I/O
with its worker threads and API calls to establish the sequence.

## 4. Derive parameters instead of copying one trace

Repeat the baseline, then change one variable at a time. Keep a control run
between experiments that might leave persistent state. Required coverage includes:

- Minimum accepted ROI, full frame, intermediate dimensions, and offset windows
  near each edge; each advertised binning mode and its coordinate convention.
- Short exposures at several durations, values on both sides of every suspected
  timing transition, and longer exposures within the harness limit.
- Gain/offset samples, their extrema and both sides of conversion-mode thresholds.
  Do not assume the ASI676 gain boundary or line period applies to the Duo.
- Each supported output format, bandwidth setting and processing control intended
  for the direct backend. Extend the harness where it currently lacks a control.
- For cooled models, temperature reads, target changes, cooler enable/disable and
  restore behavior. Record initial state and restore it after each bounded test.
- Repeated exposures in one process, reopen in a new process, then separately
  documented USB reconnect and physical cold-power startup experiments.

Diff successful control transfers within the same lifecycle phase. Infer
register fields, scaling, rounding and dependencies, then test the inferred
formula on settings that were not used to derive it. Use version-pinned SDK
disassembly to resolve dispatch targets, hidden branches and timing calculations.

The existing `extract_asi676_tables.py` intentionally accepts only a reviewed,
hash-pinned ASI676 trace and selected volatile command ranges. Do not relax that
guard or feed it another model's trace. Build a separately reviewed extractor
for a new model, excluding identity, calibration, firmware and EEPROM writes.
Unknown vendor operations remain observations until their effects are understood.

## 5. Capture and validate host processing

`--compare-wire` compares complete USB passes with the SDK's returned RAW16 image
in memory. It does not invent a decoder when lengths differ. Bulk hashing and
buffer copying affect scheduling, so run normal traces as well as instrumented
comparisons; neither is a reliable throughput benchmark.

`--trace-processing` additionally enables hooks pinned to our exact SDK 1.41
binary. The current ASI676 buffer hooks are capped at 32 MiB and did not execute
on either Duo sensor. Missing hook events mean **unknown processing**, not that
the SDK left pixels untouched. The 52,183,296-byte Duo main frame also exceeds
that snapshot cap. Locate the model's actual dispatch path and review memory
bounds before adding new hooks; a small ROI can help locate it first.

For ASI676 only, `--validate-direct-processing --compare-wire --trace-processing`
requires independently decoded factory-map indices and corrected output to match
the SDK exactly. Extend this same-frame method to each new model. Comparing two
separate dark exposures cannot establish byte parity because of sensor noise.

## 6. Gate direct acquisition on evidence

Before enabling a model, require independent initialization and complete frame
readout with the SDK absent, explicit settings round trips or traced equivalents,
validated geometry/envelope, and same-frame pixel-processing comparisons. Exercise
the parameter matrix and cold-start cases; document unsupported settings explicitly.

Then test recovery separately: idle cancellation, partial read interruption,
completed retained-frame replay, terminal transfer errors, reconnect and cooler
restoration. Drain outstanding requests before freeing buffers or resetting the
pipe. Never splice partial attempts. A deliberate host interruption is not a
USB bus fault, and retained DDR is not assumed to survive a cable or power loss.

Promote each result through **recorded → interpreted → independently implemented
→ hardware validated**, per feature and model. Store the trace provenance and a
sanitized evidence summary beside the implementation, including failed cases and
what remains unknown. See [ASI676 acquisition](sdk-free-capture.md),
[factory correction](factory-defect-correction.md) and
[Linux research / Duo observations](linux-driver-research.md).
