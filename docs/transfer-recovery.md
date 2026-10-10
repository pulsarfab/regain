# Transfer recovery and SDK gaps

The SDK remains the default backend. The direct driver uses
the installed ZWO Windows kernel driver without loading `ASICamera2.dll`.

## What can be recovered

The tested ASI2600MM Pro and ASI6200MM Pro P25 can replay a complete frame held in camera DDR
after a canceled or timed-out USB read. This avoids another exposure. It is a
restart from byte zero, not an offset-based continuation. No camera command
for seeking to an arbitrary frame offset has been established.

The implemented sequence is:

1. Finish or cancel and drain the outstanding USB request. Keep its buffers and
   `OVERLAPPED` alive until terminal completion.
2. Abort/reset the bulk pipe, stop the retained-frame sender with FPGA `18=0`,
   and allow it to settle. Verify the model's retained/idle status.
3. Set `18=1` to restart the retained sender. Read the complete frame from the
   beginning and validate its size and envelope before delivering pixels.
4. If replay fails, use another permitted read retry. Exhaustion surfaces the
   failure to the supervisor, whose replacement-exposure policy still applies.

`CancelIoEx` alone is not proof that a request has stopped; it only requests
cancellation. Windows permits normal, canceled or other error completion.
Our transport waits for terminal completion and terminates the isolated worker
if cancellation will not drain, rather than freeing driver-owned storage.
See [Microsoft's cancellation contract](https://learn.microsoft.com/en-us/windows/win32/fileio/cancelioex-func).

The ASI2600, ASI6200, ASI585, ASI662 and ASI676 direct descriptors advertise retained-frame capability.
Their configurable read retry count (default 2, maximum 5) applies at every
supported exposure length, including a 1,200-second ASI2600 exposure. The
supervisor records successful retained-read recovery separately from exposure
retries. Taking a new exposure, including after reconnect or SDK fallback,
still obeys the default 30-second cutoff. A zero read-retry count disables
retained-frame retry. An older direct host without the capability flag retains
the conservative cutoff behavior.

The guide camera's startup stream resynchronization reads a subsequent frame.
It does not advertise retained-frame capability and remains subject to the
exposure cutoff. These guarantees must not be transferred between the two
devices merely because they share an enclosure.

## Limit the size of direct USB reads

**Direct USB read size (KiB)** limits each host read request in SDK-less mode.
Set it in NINA's Advanced camera settings, native ASCOM's Timeouts tab, or the
Alpaca camera setup page. All three use the recovery key `directReadChunkKiB`.
The default is **1024 KiB (1 MiB)**; supported sizes are 1, 2, 4, 8, 16, 32,
64, 128, 256, 512 and 1024 KiB. Existing configurations keep the default.
SDK mode ignores this setting, including when SDK fallback is active.

Smaller requests can reduce throughput by adding host overhead. They do not
set a fixed MB/s cap, reserve bandwidth for other devices, change sensor timing,
or select USB 2 instead of USB 3. Request size stays fixed for an entire capture
and its retained-frame retries, including after worker replacement. The normal
download deadline and cancellation rules still apply. Native video and
continuous acquisition accept the worker's same `readChunkKiB` parameter.

For a direct CLI capture:

```powershell
regain-device zwo camera-direct --capture-585 --read-chunk-kib 64 --replay
```

ZWO's SDK **USB limit / USB Traffic** is a separate camera-side bandwidth
percentage that affects frame rate. [ZWO explains the control here](https://bbs.zwoastro.com/d/13881-what-does-the-usb-limit-control-do).
Our SDK traces show it programs an FPGA output divider; see the
[bandwidth analysis](usb2-coverage-spike.md). Direct mode retains its traced
camera-side bandwidth setting and exposes host read size separately. Its SDK
USB-limit control remains read-only; the two values have different units.

On the attached ASI585MM Pro over USB 3 on Windows, three full-resolution RAW16
captures per size and byte-identical retained replays after a partial read gave:

| Host read size | Median full-frame USB read |
| --- | ---: |
| 1 MiB | 42 ms |
| 256 KiB | 48 ms |
| 64 KiB | 48 ms |
| 16 KiB | 69 ms |
| 4 KiB | 210 ms |
| 1 KiB | 823 ms |

A 512-byte research request failed immediately, below this USB 3 endpoint's
1,024-byte packet size. Subsequent normal captures passed without a USB reset.
The option therefore starts at 1 KiB. These are short measurements on
one camera and driver, not evidence of improved fault recovery or physical
USB 2 validation. Frame size was 16,588,800 bytes; medians include six full reads
per size and exclude exposure and processing. `transfer.complete` diagnostics
record the configured size, request count and elapsed read time.

The production CLI acceptance tool repeats captures at 1, 4, 16, 64, 256 and
1024 KiB and with the size omitted. Each capture recovers a deliberately
interrupted host read and verifies a retained replay after another partial read.
Run only with an idle, operator-authorized ASI585MM Pro:

```powershell
python scripts/inspection/validate_read_chunk_size.py --hardware --output artifacts/read-size-check
```

## Hardware evidence

Tests used the attached capped ASI2600MM Pro main interface, full 6248 × 4176
RAW16, gain 100, offset 50. A trace of each owned direct CLI worker counted
exactly one `A9` exposure-start command. No SDK was loaded. These are controlled
USB request faults, not physical cable or electrical faults.

| Fault | Exposure | Result |
| --- | --- | --- |
| Cancel first pending bulk request | 0.1 s | One retained read retry; full subsequent replay had identical pixels |
| Cancel request 13 after 12 MiB | 2 s | One retained read retry; full subsequent replay had identical pixels |
| Cancel request 13 after 12 MiB | 60 s | Same frame recovered in 63.008 s total; all 12 prefix chunk interior hashes matched the reread |
| Cancel request 50, the last frame chunk | 2 s | All 49 prefix chunk interior hashes matched the reread; full subsequent replay matched |
| Pause sender after 12 MiB, issue another read | 2 s | Real read expired at 5,000 ms; cancellation/drain completed; exact prefix and full subsequent replay matched |
| Cancel request 13 with read retries disabled | 2 s | Error surfaced; no replacement exposure or false success |

All injected cancellations completed with Win32 `995`, NT `c0000120`, and USB
`c0010000`. The timeout has the same terminal cancellation codes but also
records `deadlineExpired true`. Previously, a timeout and an externally
canceled request were less distinguishable in the bulk error text.

Chunk interior hashes omit four bytes at both ends of each chunk. The timeout
test additionally compared all 12,582,908 prefix bytes after the first four-byte
envelope. Full subsequent replay comparison excludes only the two global
four-byte envelopes. Envelope counters may advance on replay and do not
provide a stable frame identifier across reconnects.

See the [sanitized evidence](transfer-recovery-evidence.json) and
[reproduction commands](../scripts/inspection/README.md#asi2600-usb-cancellation-and-timeout-experiments).
Hardware fault testing here extends to 60-second exposures. A separate NINA
test already completed a normal 1,200-second direct exposure; the policy test
uses a simulated 1,200-second request to verify that retained reads remain
enabled without allowing a replacement exposure. This is not a claim of a
1,200-second hardware fault-injection test.

## Remaining differences from the SDK

The separate [ASI6200MM Pro P25 matrix](asi6200-p25.md) also recovered a
60-second, 9576 × 6388 frame after `CancelIoEx` on bulk request 13. Its first
retained restart timed out; the second restart succeeded within the default
two-read retry budget. A subsequent replay had identical interior pixels.
Three interrupted replay tests independently exercised the additional timeout.
This unit requires a minimum 128 KiB physical readout for reliable retained
replay; the backend pads smaller ROIs and crops after factory correction.

The [ASI2600MM Pro P25](asi2600-p25.md), PID `260e`, also passed interrupted
full-frame reads and a real bulk timeout. The 12 MiB interruption took two
read retries; the forced timeout took one. Interrupted replay required one
additional restart. Recovered pixels matched without repeating the exposure.
This revision also needs the larger physical readout for small requested ROIs
and a complete sensor readout interval before freezing DDR.

The [ASI662MC](asi662mc.md) and [ASI585MM Pro](asi585mm-pro.md) also support
retained-frame reads, each validated after a 600-second exposure. Short captures
use a minimum ~100 ms sensor frame interval so that the
full image remains replayable. Its evidence distinguishes deliberate host-read
interruptions from actual USB bus faults.

| Area | Current direct implementation | Gap |
| --- | --- | --- |
| Camera coverage | ASI585MM Pro, ASI662MC, ASI676MC, ASI2600MM Pro/Duo main, ASI6200MM Pro, and ASI220MM Mini guide; see each device guide and [USB 2 results](usb2-cameras.md) | Other models/revisions need their own initialization, format and recovery evidence; a shared driver package is insufficient |
| Single-frame imaging | RAW16, ROI, gain/offset, factory correction; main bins 1–4 and long integrations | SDK format/control coverage is broader; unverified correction-map classes and modes must not be assumed equivalent |
| Acquisition modes | NINA, ASCOM, and Alpaca deliver RAW16 still frames; worker APIs add [continuous acquisition](continuous-acquisition.md) and [software WB/AWB](white-balance.md) | Direct native video covers ASI585/662/676 through 30 s and has no retained replay. Other direct models use repeated stills. Neither Regain backend captures RAW8/RGB; automatic exposure, external triggering, and ST4 are not surfaced. |
| Transfer throughput | Sequential bulk requests, configurable 1–1024 KiB (default 1 MiB), fixed camera-side USB limit 40 | SDK traces show queued overlapped transfers. Queue depth and camera bandwidth tuning need measurements and cancellation tests |
| Cooling | Temperature, target, enablement, power and supported dew controls; bounded Rust PI regulator | Capability depends on the camera: ASI585 exposes cooling but no controllable heater. It is not the SDK regulator and needs more environmental and hardware validation |
| ASI6200 auxiliary controls | Fan speed and power-LED brightness, 0–255, with readback and restoration | Momentary USB hub reset is not exposed or replayed automatically |
| Acquisition lifecycle | Observed readiness registers, retained state and framing checks; P25 cameras additionally wait a full programmed sensor frame plus 100 ms before standby | These guards are time based. Full-frame control transitions caught stale rows that valid framing and repeated dark frames did not. A definitive firmware completion indicator remains open |
| Error reporting | Structured category, progress, Windows/NT/USB or native status, and deadline flag in diagnostics and terminal protocol errors | Control/initialization failures still use ordinary error context |
| Time bounds | Configurable whole-read deadline for every transfer/replay attempt, per-request deadlines, supervisor readiness grace, worker watchdog | Cancellation drain and an in-progress control request can extend observed failure time; outer bounds still apply |
| Disconnect recovery | Windows ASI2600 P25 can reopen the handle within the read budget and verify prior pixel chunks; full reconnect restores controls/cooling and takes a replacement exposure when policy allows. [Opt-in USB reset](usb-recovery.md) can escalate on Windows/Linux | Reset abandons the retained frame. Cross-worker retained-frame adoption, physical removal, and external power cycling remain open |

For the public SDK, `ASIGetDataAfterExp` is a whole-image retrieval call with no
offset or continuation token. Inspected code and traces place USB acquisition
before public ready status; a repeated public call is not a general way to
resume camera-to-host transfer. SDK public rereads default to two attempts while ready status remains set,
independent of exposure duration. A count of zero explicitly disables them. See [SDK lifecycle findings](sdk-lifecycle.md) and
[transport inspection](transport-investigation.md).

## Next useful experiments

See [USB lifecycle work](usb-lifecycle.md) for the reopen/process-exit tests and
their reproduction scripts. Production ASI2600 P25 handle recovery now preserves
DDR and validates chunk hashes in the same worker. Cross-process verification
still requires the separate research commands.

1. Extend structured failures to control requests and record retention evidence
   alongside transfer progress. Do not stitch uncertain partial data into an image.
2. Test endpoint stalls, delayed completions and short reads separately from
   canceled requests. Establish whether each leaves replay usable. Then test
   OS device restart and USB re-enumeration without normal sensor initialization,
   which clears retained state during cleanup.
3. Compare SDK and direct behavior under the same fault and USB topology.
   Preserve the first failure details before a full reset changes camera state.
4. Prototype a small overlapped read queue only after its ownership, drain and
   replay behavior is covered. This may improve throughput; it does not by
   itself make failed transfers resumable.

No physical reset, power cycle, driver replacement or firmware change was
performed for this transfer matrix. Recovery currently relies on a responsive
device with retained DDR. A USB offload host could additionally retain a
completed frame for reliable network retrieval, but cannot recover camera
pixels that were never successfully transferred to that host.
## Cooling recovery tuning

The direct controller now resumes the last cooler output after a worker restart,
then adds 8 percentage points per degree of measured warming since the last
sample, capped at 100%. Its PI state is reconstructed from the previous output
and temperature. It ramps up by at most 8 percentage points/second and down by
12, with proportional gain 8 and integral gain 0.12. Elapsed controller time is
still capped at two seconds after blocked USB I/O; saturation stops integral
windup. Missing temperature feedback disables cooling. A disabled cooler or a
warmer requested setpoint takes precedence over restoration.

The shared Rust session restores this demand once per replacement worker,
after restoring the selected target and enable state. This applies to direct
camera capture through NINA, native ASCOM and Alpaca. SDK cooling remains owned
by the vendor SDK. Retained-frame handle recovery continues to preserve its
existing controller history.

Focused tests cover restoration, warming boost, 100% saturation, disabled and
warmer targets, repeated refreshes, malformed recovery requests, bounded response
and thermal models with different loads and actuator delays. The ASI585MM Pro
hardware probe is `scripts/inspection/validate_cooler_recovery.py`; it requires
`--hardware`, `--serial`, `--workers`, `--output`, and optionally `--target`.
It runs an owned supervisor, kills only its selected worker during a short
exposure and disables cooling before exit. Results are written as JSONL with
worker diagnostics. Hardware validation at a 15 C target reproduced the old
restart dropping 15% output to zero, warming from 14.0 to 17.6 C and taking
117 seconds to return the replacement image.
