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
