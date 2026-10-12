# Camera command and USB timing diagnostics

Camera diagnostics use **circular** recording by default. Recent command, SDK
API and USB transfer metadata stays in a separate flight recorder; healthy
captures keep NINA's INFO log concise. A fault emits the recent history as a
labelled batch, then includes five seconds of follow-up transfer activity for
cleanup and recovery. Search for `PulsarFab regain`, `REGAIN_DIAGNOSTIC` or
`diagnostics.batch.begin`. Warnings also reach WARNING; no debug-log setting
is required.

Normal SDK camera counts and cancellation of surplus read buffers are recorded
without triggering a fault batch. Failed SDK calls and acquisition failures do
trigger one.

## Live logging controls

Choose **Transfer logging** in NINA camera setup's **Advanced** tab or native
ASCOM camera setup's **Timeouts** tab:

- **circular** (default): keep recent metadata separately and emit it on a fault.
- **verbose**: forward every record to the host log.
- **off**: retain failure and recovery summaries without detailed recording.

Save while connected. Changes apply at the next safe worker command checkpoint,
including during an exposure, without reconnecting or restarting the exposure.
A command already blocked in a USB operation must finish before that checkpoint.
The shared key is `recovery.transferLogMode`. Each ASCOM slot has its own
setting; replacement workers and SDK fallback inherit the current choice.

On Windows, worker files are in **`%LOCALAPPDATA%\Regain\Diagnostics`** as
`camera-<worker PID>-current.jsonl` and `-previous.jsonl`. Each segment is capped
at 4 MiB; the in-memory recent history is capped at 8 MiB. Old records wrap.
Inactive worker logs are pruned, keeping eight pairs; active workers are kept.
The host log records the path and batch boundaries. Managed IPC and NINA API
history has a separate bounded buffer and `managed-<host PID>-fault.jsonl`
snapshot when a fault occurs. Image contents are never recorded.

Linux/macOS use `$XDG_STATE_HOME/regain/diagnostics`, falling back to
`~/.local/state/regain/diagnostics`. `REGAIN_CAMERA_DIAGNOSTICS_DIR` overrides
the directory for both worker and managed diagnostics. If disk is unavailable,
the bounded in-memory history can still be emitted into the host log. Exceptional
queue overflow is reported explicitly; recording never blocks the USB owner.

Before replying with a command failure or terminal stream error, the pipe
thread drains queued diagnostics with a two-second bound. This preserves retry
counts and fault batches when the supervisor immediately retires the worker.
A stalled log consumer can still exceed that bound; healthy replies do not wait
for diagnostics.

`REGAIN_CAMERA_TRANSFER_LOG=0` (`false` or `off`, ignoring case) remains a
host-wide override. Set it before launching the host and restart to apply it.
Remove it or set `1` to use the saved mode. SDK USB hooks are installed once so
live mode changes work, and produce no records while off; the environment
override prevents hook installation. Failure summaries remain enabled.

## Cooler power change limit

NINA's **Cooling** tab and ASCOM's **Cooler** tab include **Direct cooler maximum
power change**: 1–100 percentage points per five seconds, default **25**.
The shared key is `recovery.coolerMaxPowerChangePer5Seconds`.
Save while connected to apply at the next safe worker checkpoint. Alpaca and native Hub camera configurations use
the same keys, defaults and descriptions.

Regain's direct controller applies this rate in both directions, including
cooler startup, setpoint changes and recovery boosts. A change from 20% to 45%
uses the default five-second allowance. Blocked USB operations cannot bank
unlimited ramp credit: each update allows at most two seconds of change.
Recovery first restores the last known power baseline, then ramps any boost.
Cooler-off, disconnect and missing temperature feedback stop cooling
immediately. The vendor SDK controls its own cooler power; SDK mode ignores
this limit.

The records contain operational metadata, lengths, statuses, and timing. They
do not contain image buffers. The managed IPC logger does not serialize command
arguments or response bodies: `binaryBytes` is a length, not image content.
USB producers exclude image, EEPROM, identity, and calibration payloads.
Direct USB diagnostics may decode allowlisted short operational register
replies. SDK USB diagnostics never retain or inspect SDK-owned output buffers,
including register replies; SDK API calls can report their scalar outputs.

## Fields and correlation

Each structured record starts with `REGAIN_DIAGNOSTIC ` followed by a version-1
JSON object. NINA adds a readable prefix and keeps the entire JSON object,
including nested `details` and fields it does not recognize. Valid structured
records are not subject to the old 8 KiB truncation. Malformed or legacy
unstructured stderr remains visible, bounded to 8 KiB.

| Field | Meaning |
| --- | --- |
| `event`, `message`, `level` | Record type, readable description, and original producer severity. Even structured `debug` records reach NINA INFO. |
| `pid` | Original emitting process. Worker records keep the worker PID; managed IPC records use the NINA process PID. |
| `details.correlation` | Producer's operation identifier, paired across its submit/start and completion records. Scope it by producer PID and event family. |
| `elapsedUs` or `details.elapsedUs` | Monotonic elapsed microseconds for the recorded operation. Use this for duration comparisons. |
| `timestampUnixUs` or `details.timestampUnixUs` | UTC Unix timestamp in microseconds, useful for lining up processes. Wall-clock adjustments can affect timestamp differences. |
| `details.thread` | Producer thread identifier where available. |
| `managedContext` | Separate managed receipt/request context, described below. It does not replace worker fields. |

The Rust supervisor preserves complete worker records, including worker PID,
USB request fields, return status, and timing. Its separate supervisor context
does not replace those fields. Recovery status extracts the human failure
message for UI display rather than storing the transfer JSON in driver info.

`managedContext` contains the selected backend, `hostPid`, whether the process
is a supervisor, the camera name when known, a managed capture-attempt sequence,
the current IPC `requestId`, and its process-qualified `correlation`. Backend
context updates from acknowledged responses, including SDK fallback. Discovery
before camera selection can have no camera name.

For worker stderr, this is **receipt context**: a delayed record can arrive
while a later IPC request is active. The original worker correlation and
timestamps remain authoritative. The managed capture sequence increments on
each `start` request; it is not proof that another sensor exposure occurred.
Use the worker's exposure/start evidence for that distinction.

## Recorded operations

| Event family | Recorded boundary and metadata |
| --- | --- |
| `ipc.command.submit` | Managed request prepared for the private pipe: method, request ID, and JSON byte count. |
| `ipc.command.response` | Validated response envelope received: method, elapsed time, success flag, response JSON length, and announced binary length. |
| `ipc.command.complete` | Entire reply received, including all announced image bytes for `download`: method, elapsed time, and binary length. |
| `ipc.command.failed` | IPC operation exits with an exception: method, elapsed time, exception type, and SDK code/operation when available. |
| `nina.api.complete` | `WaitUntilExposureIsReady` or `DownloadExposure` completes: capture generation, API duration, and success/cancelled/aborted/failed outcome. Download metadata includes dimensions and pixel count when a frame is available. |
| `nina.image_factory.complete` | `CreateImageArrayExposureData` returns or throws: separate factory duration/outcome, dimensions, and pixel count. |
| `sdk.api.start`, `sdk.api.complete` | Each Regain-called SDK export, including every exposure-status poll: API name, scalar arguments, correlation, return value/scalar outputs, and completion duration. |
| `usb.submit`, `usb.submitted`, `usb.complete`, `usb.cancel` | Applicable USB/OS request stages: transport/function, endpoint or setup request, requested lengths, available completion metadata, operation correlation, and elapsed time. Direct records include transport-owned received lengths/statuses; SDK completion limits are described below. A synchronous request need not have a separate pending/submitted stage. |
| `sdk.usb.instrumentation` | Windows SDK import-hook installation result and actual patched/missing coverage. Check this before interpreting SDK USB trace completeness. |
| `usb.tracking_lost` | SDK transfer-correlation evidence became unavailable, for example because an OVERLAPPED was reused or the bounded tracker filled. |
| `asi6200.register_snapshot` | Read-only register/state evidence around programming, arming, completed readout, and failures. |

Managed command duration starts after acquiring the serialized IPC gate and
ends after receiving the full reply. It excludes time spent waiting for that
gate. For downloads, the response and complete records distinguish JSON/envelope
arrival from receiving all pixels. These durations are separate from SDK API
call time, USB kernel-request lifetime, sensor integration, and NINA image-factory
processing. NINA readiness timing includes the wait for managed capture to
finish, including its byte-to-ushort copy. NINA download timing includes its
readiness wait, metadata preparation, and image-factory call; the factory record
isolates that last call. Already-ready frames can have a short subsequent API
wait without another exposure. Overlapping USB durations must not be summed as
wall time.

NINA API records use the NINA process ID and capture generation to correlate
readiness, download, and factory boundaries. Their producer bounds descriptive
fields and records only exception type, not arbitrary exception text. Factory
timing does not inspect, copy, or hash the pixel array. Failed/cancelled calls
remain failed/cancelled; logging does not replace their original exceptions.

Windows SDK USB instrumentation wraps resolved imports inside the loaded SDK
DLL, rather than calling private offsets. It reports its coverage explicitly;
an unavailable or missing import leaves SDK behavior unchanged and makes that
coverage gap visible. The observed import set is `DeviceIoControl`,
`GetOverlappedResult`, `GetOverlappedResultEx`, `CancelIoEx`, and `CloseHandle`.
Helper DLLs and dynamically resolved functions are outside that boundary.
Wrappers preserve the original Win32 return value and last-error state.
SDK API diagnostics are cross-platform. SDK internal USB
import instrumentation is Windows-specific, so API records alone do not prove
that every internal SDK USB transaction was observed on another platform.

An SDK USB completion can carry both `elapsedUs` for the whole tracked transfer
and `callElapsedUs` for the individual Win32 call. The pending-transfer tracker
holds at most 4,096 entries and reports loss explicitly. `usb.cancel` records a
cancellation request; it does not mean the pending transfer has completed.
Use its terminal completion record when assessing the transfer lifetime.

SDK USB completion records report the Win32 BOOL result, meaningful
`win32Error`, raw `lastError`, and **`ioctlReturnedBytes`**. The last field is
the IOCTL completion count, **not an actual USB image-payload count**; the
instrumentation manifest states `ioctlReturnedBytesIsUsbPayloadCount: false`.
SDK-owned output headers, NT/USBD status, buffer-length fields, and register
reply bytes are unavailable to this observer. It stores no output-buffer
addresses for later inspection. Setup headers are read only before submission;
SDK API records such as `ASIGetControlValue` supply safe scalar replies instead.
The bundled SDK currently imports `DeviceIoControl`, `GetOverlappedResult`, and
`CloseHandle`; the optional Ex/cancel wrappers apply only when those functions
are imported. The installation manifest remains the authority for a particular
loaded SDK.

ASI6200 snapshots include geometry/exposure/offset, expected transfer size,
setup/transport generation, initialization state, raw BC register values,
snapshot duration, and any read failure. Healthy snapshots are on by default;
`REGAIN_CAMERA_REGISTER_SNAPSHOTS=0` suppresses healthy snapshots only. Failure
snapshots remain enabled. These reads are diagnostic evidence, not a new
readiness gate: transfer-count mirror meanings are not fully validated.

## Delivery and limits

USB/API diagnostic producers use a bounded asynchronous metadata queue so
pending camera DMA does not wait for NINA's log writer. The queue holds at most
8,192 records. If a receiver stops draining it, producers preserve transfer
behavior and report exceptional loss as `usb.log_dropped`; such a trace is not
complete. After USB requests drain, close attempts a bounded two-second log
flush. A failed or closed log sink cannot change command replies, pixels,
cancellation, or recovery decisions.

The managed bridge does not feed per-transfer records into
`CameraSession.Diagnostic`, `DriverInfo`, or UI status. Those remain concise
phase/recovery summaries. Full request/status/timing JSON stays in the log.
Original structured fields are retained rather than reconstructed from message
text, allowing later analysis without guessing USB status from prose.

Collect the NINA log together with the tested worker hash and full-frame
capture metadata. Compare a successful first acquisition with the following
acquisition in the same connection. For ASI6200 acceptance, use the complete
9576 × 6388 RAW16 bin-1 frame and the entire repeated/abort test sequences in
[the lifecycle workup](asi6200-sdk-parity.md#reproduce-with-full-frames-only).
The final logging build passed the full-frame acceptance described there.
Three raw-worker captures plus connection/cleanup produced about 2.1 MB of
direct logs and 9.1 MB of SDK logs. With the switch disabled, equivalent runs
produced about 9.8 KB and 1 KB respectively. These are measured examples, not
fixed per-frame limits; SDK background activity and polling change volume.

## Managed verification

Both complete managed projects passed in Release with no test filters:
**109 core tests**, and **554 NINA tests with three unrelated hardware/COM
opt-ins skipped**. The attached ASI6200 opt-in exercised direct and SDK full-frame
repeats, direct cancellation/restart, and complete INFO parsing. Coverage also
includes preserving unknown/long structured
fields, INFO delivery, independent warning delivery when the INFO sink fails,
original worker IDs/timing, metadata-only IPC records, and the live simulator's
download/camera/capture logging even when its log callback throws, plus bounded
NINA API/factory timing records for successful and unsuccessful outcomes.
Registered COM and other-camera hardware fixtures were not used for these runs.
