# Transport experiments

These tools are separate from the installed plugin. They require Windows x64,
Python 3.12 and the built debug Rust host. Select an exact SDK camera name with
`--camera-name` when multiple models are attached; otherwise exactly one camera
must be connected.
Disconnect the camera in NINA/other capture applications first.

For bringing up another sensor, follow the
[per-camera SDK capture procedure and coverage table](../../docs/camera-bringup.md).
It distinguishes captured transactions from decoded and hardware-validated support.

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
# Descriptor-only inventory when several cameras are attached:
cargo run -p zwogain-direct --locked -- --probe-all
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

`--gain` and `--offset` explicitly set those SDK controls for parameter mapping.
The pinned processing hooks also record the control dispatch targets and the
scalar sensor timing fields used by exposure configuration.

Reproduce the relevant disassembly with explicit regions (an export's first
Windows unwind entry can cover only its prologue):

```powershell
.reference/inspection-venv/Scripts/python.exe scripts/inspection/inspect_pe.py vendor/zwo/ASICamera2.dll --region 0x51d0:0x13a --region 0x16c00:0x45e --region 0x1043c0:0x300 --output artifacts/inspection/processing-disassembly.json
```

## SDK-free capture and replay

Disconnect other camera applications first. Capture is restricted to the
observed USB3 ASI676MC. Defaults: full sensor, 100 ms, gain 0, offset 10,
bin 1, USB limit 40, two retained-frame read retries.

```powershell
cargo run -p zwogain-direct --locked -- --capture --microseconds 1000000 --frames 3
cargo run -p zwogain-direct --locked -- --capture --width 512 --height 256 --x 16 --y 8 --gain 100 --offset 20
# Compare a second complete read with every byte of the original wire frame:
cargo run -p zwogain-direct --locked -- --capture --replay
# Abandon 12 MiB of a replay, restart it, and compare against the original:
cargo run -p zwogain-direct --locked -- --capture --replay-prefix-bytes 12582912
# Interrupt the first read; recover automatically without another exposure:
cargo run -p zwogain-direct --locked -- --capture --interrupt-read-after-bytes 12582912 --replay
# Set --read-retries 0 to surface that interruption instead of recovering.
.reference/inspection-venv/Scripts/python.exe scripts/inspection/validate_direct.py --long --output artifacts/inspection/direct-validation.jsonl
```

Without `--stream`, stdout contains one JSON metadata line per capture and
pixels are discarded. `--stream` emits a 32-bit little-endian JSON length,
that many UTF-8 JSON bytes, then exactly `capture.bytes` RAW16 bytes, repeated
for `--frames`. Diagnostics/errors use stderr. `capture.sha256` covers the
returned pixels; `wireSha256` covers the original transfer before its envelope
pixels are replaced. This is a research capture stream, not the production
SDK host's request/response protocol. Do not redirect binary output through
PowerShell's text pipeline; use a binary pipe reader such as `validate_direct.py`.

The hardware validator checks frame lengths, stream digests, changing capture
digests, lifecycle metadata, recovery count, retained-prefix agreement and
complete replay identity. It writes only statistics and hashes, never pixels.
The 30-second case is enabled by `--long`; normal CI runs hardware-independent
tests only. `--interrupt-read-after-bytes` explicitly injects a host-side
interruption, not a USB bus error. A replay mismatch fails the diagnostic.

For acquisition commands, packet framing, sensor retention, P25 compatibility
and limits, see [SDK-free capture](../../docs/sdk-free-capture.md).

## Direct plugin backend

The plugin packages the direct executable with an opt-in setup choice. It uses
`--serve` and the existing SDK host protocol; camera discovery in this mode also
avoids loading the SDK. The default selection continues to use the SDK host.

```powershell
# Hardware test through the production C# supervisor (statistics only):
dotnet run --project src/ZwoGain.Diagnostics -- --direct --capture --frames 3 --seconds 0.1
# Explicit host termination before download exercises reconnect and re-exposure:
dotnet run --project src/ZwoGain.Diagnostics -- --direct --capture --kill-once --frames 3
# Hardware-independent direct protocol and supervisor exercise:
dotnet run --project src/ZwoGain.Diagnostics -- --direct --simulate --capture --width 64 --height 64
```

Diagnostic logs contain camera serials. Keep them local under `artifacts/` and
publish only reviewed summaries. The simulator never opens camera hardware and
does not provide evidence for physical sensor behavior.
