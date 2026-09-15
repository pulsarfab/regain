# Architecture and protocol

```text
NINA adapter -- private pipe ----+
                                |
Alpaca client -- HTTP -----------+--> zwogain-alpaca / zwogain-core (Rust)
                                |           |
Windows COM slots -- HTTP ------+     private worker pipes
                                            |
                               zwogain-host or zwogain-direct
                                            |
                                     SDK / native USB
```

`zwogain-core` owns the recovery policy and remembered controls. The NINA pipe
adapter and standalone Alpaca server use that same controller. Each camera has
its own worker, so a failed SDK process can be replaced without losing its
settings. The old .NET recovery implementation remains for low-level diagnostics
and regression fixtures; production NINA sessions use the Rust controller.

Every Windows worker is placed in a job with kill-on-close. NINA also owns its
Rust supervisor through a job. Cancellation can terminate a hung worker without
terminating its supervisor. One session serializes capture and idle telemetry.
Discovery uses a separate process and does not open devices.

During exposure, the session refreshes temperature and cooler power every two
seconds. SDK workers read those controls directly. Native workers return a
shared copy of the readings already collected by their cooler controller, so
status requests never queue USB work behind a long capture. NINA receives these
readings in its capture-status replies; Alpaca reads the same shared state.

Alpaca serves ICameraV4, management, discovery, and setup over a loopback listener
by default. Each camera slot has a stable UUID and device number. Captures run
asynchronously, and ImageBytes streams unsigned RAW16 in ASCOM X/Y order. The
Windows .NET Framework COM driver exposes four official ICameraV4 interfaces
and forwards to Alpaca; it contains no camera recovery logic.

The Rust host never accepts network connections. Its inherited anonymous pipe
handles are private to the parent/child pair. DLL loading uses an explicit
absolute path beside the plugin. SDK C enums are represented as integers and
all buffer geometry is checked, including reading ROI/format/origin back from
the SDK before exposure. The vendor header's `long` matches Windows' 32-bit
`c_long`, even in an x64 process. On 64-bit Linux and macOS, `c_long` is 64-bit;
the same FFI declarations follow the native C ABI. Each platform's CI checks
camera and control structures against a fixture built from the vendor C header.

## Portable Rust transport

The Rust workers also run on Linux and macOS, with the same pipe protocol.
Only the NINA/COM adapters and Windows job objects are Windows-specific.
Linux/macOS camera operation still needs hardware testing.

`transport.rs` owns shared frame reads and environment controls. Its Windows
module contains SetupAPI enumeration, ZWO/Cypress IOCTLs, and overlapped I/O.
Only Windows builds depend on `windows-sys`. The native module uses `nusb`
with Linux usbfs or macOS IOKit, exclusive interface claims, vendor control
requests, and bulk-IN transfers. It never loads the camera SDK.

Each bulk transfer must finish or drain cancellation before its buffer can be
released. An undrained cancellation terminates the worker. A short or failed
chunk invalidates the whole read; existing camera-specific retained-frame
recovery decides whether to reread from byte zero. Camera timing, calibration,
image processing, and cooling logic are shared. See
[portable build and hardware testing](portable-rust.md).

## Wire version 1

The worker protocol below is distinct from the supervisor's `--stdio` adapter.
The supervisor adds recovery options to `open`, returns a status object with
phase/backend details, and attaches final capture metadata to `download`.

Each request is a 4-byte little-endian JSON length followed by UTF-8 JSON:

```json
{"version":1,"id":42,"method":"status","params":null}
```

Each response has the same framing, immediately followed by `binaryLength`
bytes of image data if present:

```json
{"version":1,"id":42,"ok":true,"result":2,"binaryLength":0}
```

Errors carry `error`, `sdkCode`, and `sdkOperation`; error responses never
contain pixels. Requests/responses are bounded to 64 KiB of JSON. Frames are
bounded to 512 MiB and must match width × height × 2 exactly. A download response
contains `{width,height,bytes}` and contiguous little-endian unsigned RAW16
pixels. No JSON pixel arrays, base64, per-pixel RPC, or temporary image files are
used. This trades a few bulk memory copies for simpler frame ownership than
shared memory, and accommodates the ASI6200's 122,342,976-byte frame (exact size
is computed from dimensions, never a fixed camera-specific allocation).

Direct responses additionally report `readRecoveries`. A successfully validated
frame may include `cleanupError` if stopping/resetting the camera afterward
failed. The frame remains usable; the direct server rejects another `start`
until the connection is reopened. Environment controls remain available while
the supervisor replaces the worker. Research CLI runs emit the valid frame and
then stop on this condition.

Methods: `list`, `open` (name and optional serial), `get`, `set`, `start`
(width, height, bin, x, y, microseconds, dark), `status`, `download`, `stop`,
`close`. `fault` and `simulation` exist only with `--simulate`; a real SDK host
rejects both. Responses echo request IDs; mismatches, truncated payloads or
bad bounds invalidate the worker. Standard error is diagnostics only.

Worker stderr may contain `ZWOGAIN_DIAGNOSTIC ` followed by a JSON record with
`version: 1`, `level`, `event`, `message`, and `pid`. These records never appear
on stdout or change a command result. NINA sends retry/failure events to its
Warning log, recovery events to Info, and routine frame delivery to Debug.
Plain stderr, including SDK loader errors, is kept at Info. No diagnostic path
uses NINA notifications or error dialogs. Logger failures are ignored so they
cannot interrupt capture. Terminal capture failures still follow the normal
error contract when recovery is exhausted.

USB read failures are logged as they occur, including the retry count, cause,
and whether recovery rereads a retained frame or advances the guide stream.
The supervisor logs fallback decisions, replacement exposures, restored
controls/cooling, successful recovery, and final failure without an image.

## Time bounds and recovery

The parent independently bounds each command (15 seconds), download (60
seconds), SDK exposure readiness (requested duration + 30 seconds), USB delay (5
seconds), cooling settle (300 seconds), and total retry count (3 by default).
Retries apply only when the requested exposure is no longer than
`MaximumRetryExposureSeconds` (30 seconds by default, inclusive). Longer
exposures are permitted. Ready-frame rereads are independent of this cutoff;
setting it to zero disables replacement exposures. Direct guide stream
resynchronization is subject to the cutoff because it reads a subsequent frame.

Direct readiness includes USB readout and processing, so its budget is exposure
duration + exposure grace + `(readRetries + 1) * DownloadTimeoutSeconds`.
The parent sends `captureTimeoutSeconds` with one additional command-timeout
margin for the worker watchdog. NINA's outer timeout includes the same allowance.
The supervisor still separately bounds the final image transfer over IPC.
The SDK call does not have to cooperate with cancellation. Polling is 25 ms;
the SDK itself reports its internal capture/transfer failure as an exposure
state in many cases, before the application calls `ASIGetDataAfterExp`.

Control snapshots explicitly exclude auto-exposure controllers, USB hub reset,
GPS and other action-like controls. Requested changes are retained separately
from observed temperature/power. Applied values are cached per host to avoid
rewriting unchanged controls on each short exposure, but all are replayed and
read back after a host replacement. Cooler enablement is restored after the
target. Cooling only gates recovery if it was enabled before failure. The
original temperature reference is retained across all attempts.

Permanent SDK size/format/parameter errors are surfaced immediately; camera
removal, closed/invalid ID, timeout, failed state and generic SDK errors use
the bounded recovery path. Cancellation never spends retry budget or starts a
replacement exposure. After cancellation or exhaustion, the supervisor makes a
bounded background attempt to reconnect and restore controls, without an
exposure. Idle telemetry can retry a failed control connection later. The
original thermal reference survives this idle reconnect and gates the next
capture. Unavailable control connections expose an error state and unknown
temperature/power rather than stale telemetry. Disconnect also attempts cooler
shutdown by serial when the direct worker has already died. At retry exhaustion
the final capture exception includes the last failure and attempt count.
No failed/partial frame reaches NINA.
