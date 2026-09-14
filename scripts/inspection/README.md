# Transport experiments

These tools are separate from the installed plugin. They require Windows x64,
Python 3.12, the built debug Rust host, and exactly one connected ASI camera.
Disconnect the camera in NINA/other capture applications first.

From the repository root:

```powershell
python -m venv .reference/inspection-venv
.reference/inspection-venv/Scripts/python.exe -m pip install -r scripts/inspection/requirements.txt
cargo build --locked

.reference/inspection-venv/Scripts/python.exe scripts/inspection/inspect_pe.py vendor/zwo/ASICamera2.dll C:/Windows/System32/drivers/ASICAMUSB3.sys --output artifacts/inspection/pe.json

.reference/inspection-venv/Scripts/python.exe scripts/inspection/trace_transport.py --output artifacts/inspection/roi.jsonl
.reference/inspection-venv/Scripts/python.exe scripts/inspection/summarize_trace.py artifacts/inspection/roi.jsonl
.reference/inspection-venv/Scripts/python.exe scripts/inspection/probe_driver.py artifacts/inspection/roi.jsonl
```

The tracer starts and attaches Frida to its own Rust host. It never attaches to
NINA or an existing process. It records SDK entry/exit, camera-handle IOCTLs,
fixed transport headers, completion byte counts and errors. The observed `0xBC`
register-read responses are limited to four bytes. Normal exposure/initialization
commands still go through the vendor SDK. Default capture: one 100 ms,
512 × 256 RAW16 frame; 60-second whole-process deadline. Output files must be new.

For the ASI676MC full frame:

```powershell
.reference/inspection-venv/Scripts/python.exe scripts/inspection/trace_transport.py --output artifacts/inspection/full.jsonl --width 3552 --height 3552 --seconds 1
```

Explicit fault experiment (one pending bulk request cancelled with `CancelIoEx`):

```powershell
.reference/inspection-venv/Scripts/python.exe scripts/inspection/trace_transport.py --output artifacts/inspection/cancel.jsonl --width 3552 --height 3552 --seconds 1 --cancel-first-bulk
```

Check the `injected-cancel` event and the subsequent completion, not just the
process exit code. Cancellation can lose a race to completion. An SDK failed
exposure is an expected recorded outcome in this mode. The tool does not do
wrapper retries, so an eventual successful frame exposes SDK-internal recovery.

Add `--hash-bulk` to compare data across transfer passes. Completed bulk buffers
are passed to the local Python tracer, SHA-256 hashed, and discarded. Only
digests/lengths are saved; no pixel files are written. This extra copying and
hashing affects timing. Even passive instrumentation changes scheduling; do not
use these runs as throughput benchmarks.

`probe_driver.py` uses a camera interface path observed in a previous trace. It
opens the installed driver, queries its version and standard device/configuration
descriptors, then closes the handle. It does not load the ASI DLL, issue vendor
commands or bulk reads, reset hardware, or change driver bindings. It has a
30-second subprocess deadline. This probe is specific to the observed Cypress
IOCTL ABI; it is not a generic arbitrary-driver IOCTL utility.

Raw traces include local device interface paths and are ignored under
`artifacts/`. Keep these local. Publish reviewed summaries instead. The research
dependencies and hooks are not packaged with the plugin.

See [findings and implementation plan](../../docs/transport-investigation.md).

## Direct Rust driver probe

Disconnect the camera from NINA and other applications first. This executable
uses the installed driver exclusively and never loads ASICamera2.dll:

```powershell
cargo run -p zwogain-direct --locked
cargo run -p zwogain-direct --locked -- --probe
# Explicit idle-endpoint experiment: one 16 KiB read, cancel after 100 ms, drain.
cargo run -p zwogain-direct --locked -- --probe --cancel-read
```

Without arguments it reports the number of interfaces, opening none. Probe mode
requires exactly one interface. It validates standard descriptors and prints
status metadata. The optional read discards all received bytes and never treats
them as an image. It does not issue vendor writes, start exposures or reset pipes.
The process has a 30-second watchdog and terminates if a cancelled request will
not drain; this prevents returning/freeing memory still owned by the driver.

## Pixel mapping and processing observation

```powershell
.reference/inspection-venv/Scripts/python.exe scripts/inspection/trace_transport.py --output artifacts/inspection/mapping.jsonl --width 3552 --height 3552 --compare-wire --trace-processing
.reference/inspection-venv/Scripts/python.exe scripts/inspection/trace_transport.py --output artifacts/inspection/replay-mapping.jsonl --width 3552 --height 3552 --seconds 1 --compare-wire --trace-processing --cancel-first-bulk
.reference/inspection-venv/Scripts/python.exe scripts/inspection/summarize_wire.py artifacts/inspection/mapping.jsonl artifacts/inspection/replay-mapping.jsonl --output artifacts/inspection/reviewed-mapping.json
.reference/inspection-venv/Scripts/python.exe -m unittest discover -s scripts/inspection -p "test_*.py"
```

`--compare-wire` retains a bounded amount of pixel data in memory (up to 256 MiB
of completed transfers per exposure; output frame at most 128 MiB), then emits
only equality/transform statistics. Temporary arrays increase peak memory usage.
It splits consecutive completed requests at missing/cancelled requests and never
joins data across those gaps. Equality across complete runs provides evidence
about replay, not an acceptance policy for camera images. `--bin`, `--x` and
`--y` vary binning and the binned ROI origin; size mismatches are reported without
inventing a decoder.

`--trace-processing` additionally observes three internal buffer stages and the
retrieval dispatch target. It requires `--compare-wire` and the exact SHA-256 of
the inspected Windows x64 SDK 1.41 binary. Snapshots are capped at 32 MiB each.
The hooks are passive, version-specific research observations, never production
dependencies or calls to undocumented entry points. No buffers are modified.
Neither raw transfer data nor processing snapshots are written to files.

Reproduce the relevant disassembly with explicit regions (an export's first
Windows unwind entry can cover only its prologue):

```powershell
.reference/inspection-venv/Scripts/python.exe scripts/inspection/inspect_pe.py vendor/zwo/ASICamera2.dll --region 0x51d0:0x13a --region 0x16c00:0x45e --region 0x1043c0:0x300 --output artifacts/inspection/processing-disassembly.json
```
