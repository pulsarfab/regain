# Transfer recovery and SDK gaps

Status: 2026-09-14. The SDK remains the default backend. The direct driver uses
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

The ASI2600, ASI6200 and ASI676 direct descriptors advertise retained-frame capability.
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

| Area | Current direct implementation | Gap |
| --- | --- | --- |
| Camera coverage | Verified ASI676MC USB3, ASI2600MM Pro main USB3, ASI6200MM Pro P25 USB3 (`620b`), ASI220MM Mini guide USB2 | Other models/revisions need their own initialization, format and recovery evidence; a shared driver package is insufficient |
| Single-frame imaging | RAW16, ROI, gain/offset, factory correction; main bins 1–4 and long integrations | SDK format/control coverage is broader; unverified correction-map classes and modes must not be assumed equivalent |
| Additional SDK modes | NINA path delivers RAW16 still frames | RAW8/RGB, live video, automatic controls, white balance/gamma, flip, external triggering and ST4 are not implemented/surfaced by the direct path; availability in the SDK varies by model |
| Transfer throughput | Sequential 1 MiB bulk requests, fixed USB limit 40 | SDK traces show queued overlapped transfers. Queue depth and bandwidth tuning need measurements and cancellation tests |
| Cooling | Temperature, target, enablement, power and dew control; bounded Rust PI regulator | It is not the SDK regulator and needs more environmental and hardware validation |
| ASI6200 auxiliary controls | Fan speed and power-LED brightness, 0–255, with readback and restoration | Momentary USB hub reset is not exposed or replayed automatically |
| Acquisition lifecycle | Observed readiness registers, retained state and framing checks; ASI6200 additionally waits a full programmed sensor frame plus 100 ms before standby | These guards are time based. ASI6200 full-frame control transitions caught stale rows that valid framing and repeated dark frames did not. A definitive firmware completion indicator remains open |
| Error reporting | Win32, NT, USB status, chunk number and deadline flag in diagnostics | Errors are still strings, not structured transport categories exposed through the host protocol |
| Time bounds | Per-request deadlines, supervisor readiness grace, worker watchdog | No dedicated configurable whole-transfer/replay deadline. Outer limits can terminate before every configured retry is used |
| Disconnect recovery | Reconnect, restore controls/cooling, and take a replacement exposure when policy allows | Retained pixels surviving device reset, removal, power loss or worker replacement are unproven |

For the public SDK, `ASIGetDataAfterExp` is a whole-image retrieval call with no
offset or continuation token. Inspected code and traces place USB acquisition
before public ready status; a repeated public call is not a general way to
resume camera-to-host transfer. SDK public rereads default to two attempts while ready status remains set,
independent of exposure duration. A count of zero explicitly disables them. See [SDK lifecycle findings](sdk-lifecycle.md) and
[transport inspection](transport-investigation.md).

## Next useful experiments

1. Record structured transport failures and phase transitions, including bytes
   successfully received and whether retention was still reported. Do not
   stitch uncertain partial data into an image.
2. Test endpoint stalls, delayed completions and short reads separately from
   canceled requests. Establish whether each leaves replay usable. Then test
   close/reopen and USB re-enumeration without normal sensor initialization,
   which currently clears retained state during cleanup.
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
