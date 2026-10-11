# ASI6200 SDK lifecycle and readout parity

The direct ASI6200 path now follows the observed SDK 1.41 connection lifecycle,
sensor start sequence, and queued first-frame USB timing. Full 5-second RAW16
captures on the tested original camera took 6.297–6.313 seconds, compared with
6.250–6.422 seconds through the SDK and about 7.8 seconds with the
0.6.0.3/0.6.0.4 direct workers. These are local acceptance results, not a
reproduction of the reported remote second-frame failure.
The mapping covers the observed normal lifecycle and timing; it does not claim
branch-by-branch equivalence with SDK error handling or internal caching.

This work follows the [cancellation and clock-restoration fix](asi6200-abort-cleanup.md).
The [original-camera workup](asi6200-original.md) and
[USB register reference](camera-usb-registers.md) describe the earlier evidence
and the limits of the inferred register roles. SDK evidence is from version
1.41, DLL SHA-256
`0c8778c3cce2012961b079e3c7d0d8348a8b3823939335d9e98148cb5d5dc34a`.
Production direct capture does not load that DLL or call private SDK functions.

## Connection and capture sequence

| Stage | Direct behavior and SDK comparison |
| --- | --- |
| Selected connection opens | Read factory ASID with `BE=0`, `C3` reads, then `BE=1`; validate and decode before `AF` and sensor initialization. Discovery does not initialize a camera. |
| Initialize once | Apply the detected revision's initialization and RAW16 tables before acknowledging open. Cache immutable ASID and one decoded physical ROI map for the connection. |
| Thermal state | Restore acknowledged actuators during initialization, before RAW16. After RAW16, verify physical state using reads; repair it only after a detected reset or uncertain setup. Healthy setup no longer adds the unconditional post-RAW16 actuator rewrite bundle introduced in 0.6.0.4. |
| Each requested capture | Program physical ROI, FPGA transfer count, gain, offset, and exposure timing. Reuse initialized sensor state. Repair stale clock gates and clear the previous DDR frame before arming. |
| Arm sensor | Send `A9`; read/modify/write `BD00` to assert bit 4; write `B6:019e=1`; read `B7:0000` and set bit 2; wait **10 ms**; read `B7:0000` again and clear bit 0; read/modify/write `BD00` to clear bit 4; drain/reset the host pipe. Preserve unrelated flags. |
| Queue first-frame reads | Submit one complete frame before the host-timed handshake. At the default 1 MiB chunk size, a full frame uses **117 requests**: 116 × 1,048,576 bytes plus 708,160 bytes. |
| Host-timed exposure | For exposures of at least one second, wait **15 ms** after queuing, then issue complete 5 ms high / 20 ms low synchronization pulses. Preserve the SDK integration and clock-restoration ordering. The added 15 ms constant is ASI6200-specific; ASI2600 timing is unchanged. |
| Successful normal readout | Overlap USB reception with exposure/readout, require the complete byte count, then freeze the sensor. No additional full-frame readout guard is applied to this successful queued path. |
| Correct and deliver | Validate the envelope, apply factory correction, crop/software-bin when required, and report the corrected image hash. Finish ordinary stop/DDR discard and host-pipe cleanup before reusing capture. |

A USB, initialization, or cleanup fault invalidates reusable setup. The next
capture revalidates calibration and initializes again. Clean cancellation with
successful cleanup preserves initialized state. ROI changes replace the one
cached correction map; they do not accumulate maps or retain image pixels.

Clock restoration remains unconditional on host-timed errors and cancellation:
clear `BD19` bit 0, wait 100 ms, resume `B6:019e=1`, wait 100 ms, then clear
`BD0b` bits 4 and 0. Readbacks must confirm the gates are open. A failed
restoration cannot acknowledge a clean abort. Cooler/heater flags survive
these read-modify-writes, and disconnect switches both actuators off.

## Bounded queue and retained-frame recovery

Windows OVERLAPPED and native USB batches own their buffers until every request
has terminally completed. Cancellation and errors cancel/drain pending I/O
before those buffers can be freed. Queue depth is bounded at **128 requests**;
smaller configured chunks refill slots only within the requested frame.
The default full-frame batch therefore has depth 117.

SDK traces replenish completed slots with speculative reads for a subsequent
frame, then cancel those reads on stop. Direct capture queues exactly the
requested frame and does not add that speculative next-frame traffic. The
first-frame queue placement and sensor timing match the observed SDK path.

On a recoverable USB read fault, cancel/drain the first-pass batch, allow the
known sensor interval to finish when necessary, and freeze DDR before bounded
same-frame replay. Record SHA-256 continuity evidence for every completed chunk
and require replayed pixel bytes to match it. The changing envelope counters
are excluded from this comparison. A full byte count and valid envelope remain
mandatory; a partial frame is never returned as an image. Handshake/control
failures and cancellation do not become downloadable retained frames.
Replay also requires the exposure callback to have completed successfully and
the failure to carry structured USB-read evidence. The `exposure_complete` gate
stays false until that callback succeeds. A queue-submission failure before
exposure, an incomplete exposure callback, or cancellation cannot replay an
older DDR frame merely because a transport error was reported. Pending I/O is
still cancelled/drained and ordinary cleanup runs on those exits.

The conservative sensor-interval guard remains for fault recovery and explicit
retained-sender research. It is not an extra delay on successful normal capture.
`downloadPipeline: "sdk-queued"` identifies the normal first-pass path;
`retained-research` identifies explicit interruption/timeout research. The
`readoutGuardUs` metadata describes the available recovery guard, not an
additional healthy-path wait.

The output wrapper reuses the corrected native image hash when pixels are
unchanged and recomputes it after crop/bin. Chunk continuity adds an intentional
hash pass to protect retained recovery. Short-read diagnostics now report
whether the received suffix contained an ASI trailer and its sequence; this is
failure evidence, never permission to accept fewer bytes. BC register snapshots
are diagnostic only because the transfer-count mirror semantics still require
hardware confirmation.

## Remaining SDK differences

The direct path deliberately queues only the requested frame, while the SDK
replenishes speculative next-frame requests. ROI and control registers are
still programmed for each direct capture; the SDK may reuse cached values
when those settings do not change. Direct capture also retains explicit gate
verification, thermal checks, diagnostic snapshots, and pixel-continuity
hashing needed for its recovery guarantees.

The shared synchronization loop has a pre-existing boundary difference: direct
capture accepts a successful fifth pulse, while the inspected SDK 1.41 worker
rejects success at its fifth-attempt counter check. No regression evidence
implicates that edge, and the local hardware tests did not force synchronization
to succeed only on pulse five. The normal first-success pulse ordering and
ASI6200 15 ms initial delay are the verified timing mapping; this boundary is
not claimed as SDK-equivalent behavior.

## Measured acceptance

Tests used the attached original ASI6200MM Pro, revision register **3**, on
Windows USB 3. Every acquisition in this acceptance uses the complete
**9576 × 6388, 16-bit, bin-1 frame: 122,342,976 bytes**. Worker timing, raw
cancellation, retained-replay and NINA repeat tests use gain 100 and offset 50
unless the paired-offset freshness check explicitly changes offset. The NINA
abort API test preserves its acknowledged default gain/offset settings.

Times run from the worker start request through delivery of all pixels to the
harness. Connection/open setup and harness hash/statistics work are outside
that interval. Builds used optimized workers; diagnostic phase timestamps
reflect stderr receipt and include logger scheduling.
The 0.6.0.4 baseline is an optimized local build of the same pre-change source,
not a benchmark of a signed 0.6.0.4 package.

| Worker/backend | Full 5-second captures | Start to delivered pixels |
| --- | --- | --- |
| Signed 0.6.0.3 direct | 3 | 7.78–7.91 s |
| 0.6.0.4 source baseline, optimized local build | 3 | 7.83–7.84 s |
| SDK parity candidate, cooler/heater enabled | 4 | 6.297–6.313 s |
| SDK 1.41 | 3 | 6.250–6.422 s |

| Full-frame exposure | Direct candidate | SDK 1.41 |
| --- | --- | --- |
| 0.1 s | 1.578–1.609 s | 1.547–1.594 s |
| 1 s | 2.156–2.157 s | 2.125 s |

Paired full-frame offset 50/100 captures shifted every row's median by 500 ADU,
supporting the row-freshness check. Independent captures have different noise
and hashes; SDK pixel equality is only claimed when comparing the same captured
USB frame, as in the earlier factory-processing workup.

Two additional full 5-second frames with **1 KiB USB chunks** completed in
6.546 and 6.469 seconds with no fault or retry. Offset 50/100 checks covered all
rows. These full images exercise batch refill at the bounded depth of 128;
the smaller transfer chunks do not change image geometry.

Completed NINA API acceptance delivered three consecutive full 5-second images
through one `ResilientCamera`, supervisor, and raw worker. The separate full-frame
API abort test passed caller cancellation after 170 ms and `AbortExposure` after
two seconds, each followed by a full 5-second image. Across these two tests,
six full images and two actively cancelled full-frame exposures kept stable
process identities, with zero recaptures/read retries and no SDK fallback.
The factory checked pixel-array length, geometry, depth, and capture metadata;
the repeat test also checked changing hashes and row statistics.
All 6,388 row fingerprints changed in both consecutive frame comparisons.
The final optimized-worker rerun passed both API tests in 49.38 seconds.
The repeated 5-second frames became ready in 6.363–6.414 seconds;
`DownloadExposure`, including factory hashing/statistics, took 108–125 ms,
for 6.474–6.534 seconds total. These API timings are separate from the
raw-worker start-to-pixels measurements above. Both frame pairs again changed
all 6,388 row fingerprints, kept stable workers with no SDK/retry/reconnect,
and left thermal controls off after disconnect.

The updated raw acceptance matrix contains eleven active stages: immediate,
170 ms after start, initialization, exposure start, synchronization, integration,
620/820/1020 ms into integration, clock restoration, and downloading. All
cancelled acquisitions and the two offset baselines are full frame; every
follow-up is a full 5-second capture. The complete **11/11 matrix passed** in
one raw worker, with fresh paired-offset response on every row, no SDK load,
device reopen, read retry, or replacement exposure. Stop acknowledgements took
15–344 ms in the final rerun. After disconnect, physical `BC19=0x80` with
mask `0xc0` confirmed that both cooler and heater were off.
The obsolete separate sensor-readout stage is not emitted by normal queued
capture and is no longer a test case.

Both recorded full matrices contained exactly one `AF` initialization, two
`BE` requests (one factory-read entry/exit pair), and 22 `A9` starts across
baselines, cancelled acquisitions, and follow-ups. Clean abort/reuse therefore
did not trigger repeated sensor initialization or calibration reads. The final
matrix tested worker SHA-256
`b00095b52a7a00d62063dc2c1ac1f74cff8eb1f1fae3674c76aa1f1fb07cf984`.

A separate full-frame bulk-cancellation exercise cancelled request 64 during
the first of two 5-second captures and requested full retained replay. The
first frame recovered after two retained retries; the first retained restart
timed out, an already observed behavior on this original camera. The completed
63 MiB prefix matched on recovery, excluding envelope counter bytes, and full
same-frame replay returned identical pixels. The second capture needed no
retry. Both frames used one worker and two total `A9` starts. Two total `AF`
initializations showed that the USB fault invalidated setup before the second
capture. No image was accepted partially and no replacement exposure was used.

The companion request-1 cancellation also passed two full 5-second captures
with full retained replay. The first capture used two retained retries, the
second none, and both frozen replays returned identical pixels. With the first
chunk cancelled, there were no accepted first-pass pixels to compare:
`retainedPixelsVerified` correctly remained false. The request-64 run reported
true because its completed prefix supplied continuity evidence. Both runs used
two total exposure starts and no SDK fallback.

A separate full-frame submission-failure check used a complete 5-second
baseline, then made the first armed bulk request use an invalid handle before
the kernel call. Windows rejected it synchronously with error 6; no request
was queued for that call. Capture failed in **110 ms**, before synchronization
or integration, with no pixels or retained retry despite a configured retry
budget of two. The conservative failed-capture contract then required one
explicit close/open of the same camera in the same worker. A fresh full
5-second image succeeded with one new initialization and no SDK fallback.
Both closes physically confirmed cooler/heater shutdown. This is connection
recovery after a real submission failure, not same-frame retained replay.

The final Rust all-targets run passed **247 tests**: 147 `regain-zwo` unit tests
and 32 integration tests, plus 64 `regain-core` unit tests and four integration
tests. Windows Clippy and the Linux all-targets compile check passed. The
exposure-completion replay gate has regression coverage. The managed core suite
passed all 105 tests. The complete `Regain.NINA.Tests` project passed 536 tests
with five opt-ins skipped: the two ASI6200 hardware tests, the two ASI585 hardware
theory groups, and the registered Hub COM fixture. Both complete managed suites
ran in Release without filters or hardware/COM opt-ins. The two ASI6200 opt-ins
passed separately; an ASI585 was not attached. The cooling hold fix accepts
legitimate zero cooler demand at/below the restored setpoint without discarding the required
continuous stability interval; invalid/missing telemetry still resets it.
New queued hardware acceptance was on the original Windows camera. P25 and
native-platform hardware acceptance remain separate work.

## Reported failure and limits

The remote 0.6.0.4 report is valid evidence from a different physical camera.
Its second acquisition repeatedly stopped at **67,554,880 bytes**, including a
446,016-byte short transfer at the reported chunk 64. A later second acquisition
timed out with zero bytes. Reconnecting allowed replacement captures. Earlier
accepted images included both zero-star and nonzero-star results.

Neither exact failure reproduced on the attached original camera. Repeated
initialization/calibration, actuator writes after RAW16, start timing, and
sequential reads were concrete SDK divergences; removing them does not prove
which, if any, caused the remote truncation. This change establishes measured
SDK parity and preserves error evidence for another report. It does not claim
remote causal proof, illuminated-image performance, cold-power startup, or
physical unplug retention. Keep serials, device paths, calibration, and raw
USB traces private; this document contains only sanitized summaries.

## Reproduce with full frames only

Disconnect NINA and every other camera owner before starting a hardware test.
Use an optimized worker and a new output directory. Build the shared supervisor
and raw worker together:

```powershell
cargo build --release --locked -p regain-device -p regain-alpaca
$workers = (Resolve-Path target/release).Path
```

For three consecutive full images without read recovery, the CLI requires
exactly one matching ASI6200 interface. It prints metadata/hash JSON unless
`--stream` is explicitly requested; do not add that option for this check.

```powershell
& "$workers/regain-device.exe" zwo camera-direct --capture-6200 `
  --width 9576 --height 6388 --bin 1 --microseconds 5000000 `
  --gain 100 --offset 50 --frames 3 --read-retries 0 --read-chunk-kib 1024
```

Run the entire serial-pinned abort matrix, including paired offset baselines,
using an inspection Python environment with Frida and NumPy. Do not pass
`--case` or `--skip-freshness-baseline` for aggregate acceptance. Every request
is full frame; there is no small-ROI choice in this harness.

```powershell
$inspectionPython = 'C:\path\to\inspection-venv\Scripts\python.exe'
$serial = '<selected-camera-serial>'
& $inspectionPython scripts/inspection/validate_6200_abort.py --hardware `
  --worker "$workers/regain-device.exe" --serial $serial `
  --output artifacts/asi6200-sdk-parity-NEW/abort --thermal
```

The matrix bounds phase waits, stop acknowledgements, fresh captures, downloads,
and shutdown. It requires active cancellation, an idle camera afterward, one
worker, no SDK/device reopen/recovery, physical thermal readbacks, and fresh
per-row offset response. Preserve its private `results.json`, worker log, and
USB trace together with the tested binary hash.

For the two NINA API tests, the opt-in fixture pins the lab camera identity in
source; verify that fixture before running on another camera. Select the same
optimized directory for both supervisor and worker:

```powershell
$env:REGAIN_TEST_HARDWARE_WORKERS = $workers
$env:REGAIN_TEST_ASI6200_HARDWARE = '1'
dotnet test tests/Regain.NINA.Tests/Regain.NINA.Tests.csproj `
  -c Release `
  --filter 'FullyQualifiedName~Asi6200DirectFullFrameRepeatThroughNina|FullyQualifiedName~Asi6200DirectEarlyAndGatedAbortRestartThroughNina'
```

`REGAIN_TEST_WORKERS` is the fallback worker-directory override. The tests
use bounded cancellation tokens, check thermal state, and switch cooler/heater
off in `finally`. They exercise the NINA camera API and image factory without
changing an installed plugin or controlling the NINA UI.
