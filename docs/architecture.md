# Architecture and protocol

```text
NINA ICamera adapter / equipment provider
             |
CameraSession supervisor (.NET, no vendor DLL)
             |
private inherited stdin/stdout pipes
             |
zwogain-host.exe (Rust, one SDK-owning thread)
             |
ASICamera2.dll -> ZWO Windows driver -> camera
```

The supervisor must survive SDK faults. Placing it in the plugin keeps the
camera DLL outside NINA while letting NINA cancellation interrupt even a hung
native call. Every host is placed in an unnamed Windows job with kill-on-close;
NINA exit/crash closes the job and kills orphaned workers. A killed SDK host
cannot corrupt the supervisor's saved settings or return an old frame after
reconnection. Discovery uses a separate short-lived process and does not open
devices. One connected session owns one host and serializes capture and idle
telemetry/control transactions.

The Rust host never accepts network connections. Its inherited anonymous pipe
handles are private to the parent/child pair. DLL loading uses an explicit
absolute path beside the plugin. SDK C enums are represented as integers and
all buffer geometry is checked, including reading ROI/format/origin back from
the SDK before exposure. The vendor header's `long` matches Windows' 32-bit
`c_long`, even in an x64 process.

## Wire version 1

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

Methods: `list`, `open` (name and optional serial), `get`, `set`, `start`
(width, height, bin, x, y, microseconds, dark), `status`, `download`, `stop`,
`close`. `fault` and `simulation` exist only with `--simulate`; a real SDK host
rejects both. Responses echo request IDs; mismatches, truncated payloads or
bad bounds invalidate the worker. Standard error is diagnostics only.

## Time bounds and recovery

The parent independently bounds each command (15 seconds), download (60
seconds), exposure readiness (requested duration + 30 seconds), USB delay (5
seconds), cooling settle (300 seconds), and total retry count (3 by default).
Retries apply only when the requested exposure is no longer than
`MaximumRetryExposureSeconds` (30 seconds by default, inclusive). Longer
exposures are permitted but their first failure is surfaced. The same threshold
also suppresses optional same-frame re-download attempts; elapsed download or
recovery time does not affect eligibility. Setting the threshold to zero
disables all automatic capture retries.
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
new host on its own. At retry exhaustion the final exception includes the
last failure and attempt count. No failed/partial frame reaches NINA.
