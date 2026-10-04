# ZWO Direct USB 2 coverage spike

2026-10-03. **Not a universal hardware sign-off.** The descriptor fallback in
PR #11 is necessary and passed ASI662MC/ASI676MC hardware tests, but this spike
found additional SDK output-pacing behavior that Regain does not yet reproduce.
Do not describe accepting a USB 2 descriptor as proving successful capture on
every camera. The static audit opened/enumerated no cameras. The subsequent
operator-authorized P25 hardware validation is recorded below; no port resets
were performed.

## Scope and evidence matrix

The production `server::Model::ALL` inventory has six identities and seven
hardware variants. It is not the full ZWO SDK camera catalog. For example,
ASI183, ASI533, ASI2600MC and ASI6200MC have no Regain direct profile; USB 2
fallback cannot provide missing sensor initialization/processing drivers.
Older/native USB 2 cameras must not be assigned a modern FX3 profile by name.

| Current direct identity | Variant | USB 2 evidence | Remaining gate |
| --- | --- | --- | --- |
| ASI662MC / `662b` | Current profile | Windows capture, replay, interrupted reads, short ROI and 30 s exposure passed | SDK output-rate comparison, cold start, actual bus faults |
| ASI676MC / `676d` | Current profile | Same, plus production worker reopen; short ROI required sensor pacing fix | SDK output-rate comparison, cold start, actual bus faults |
| ASI2600MM Duo / `2601` | Original main sensor | Windows USB 2 and USB 3: each 40 cases / 42 frames plus tiny-edge ROI and production recovery passed on the documented builds | SDK output-rate comparison, cold start and actual bus faults |
| ASI2600MM Pro / `260e` | P25 | Windows USB 2: 40 cases / 42 frames, all-row freshness, bins/ROI, retained recovery, production cancellation/reopen passed | SDK output-rate comparison, cold start, actual bus faults; do not reuse original timing |
| ASI6200MM Pro / `620b` | Original, BC:1c = 3 | Windows full frames, every-row offset freshness, bins/ROI, replay/recovery and 30 s exposure passed; strict high-gain row-uniformity gate failed | High-gain characterization, SDK output-rate comparison, cold start and actual bus faults; not an unconditional matrix pass |
| ASI6200MM Pro / `620b` | P25, BC:1c = 5 | Windows full frames, bins/ROI, retained/worker recovery and capped every-row offset transitions passed; strict gain-700 row-uniformity gate failed | High-gain characterization, SDK pacing, cold start and actual bus faults; not an unconditional matrix pass |
| ASI220MM Mini / `2209` | Duo guide | Existing native USB 2 acquisition evidence | Streaming resynchronization, not retained replay; zero-line short exposures remain rejected |

See [USB 2 hardware results](usb2-cameras.md) and [guide workup](duo-capture.md).
All new Linux/macOS USB 2 hardware paths remain unvalidated. A shared enclosure,
PID, USB transport or SDK class family does not merge the evidence rows.

### P25 follow-up hardware result

After the static audit, the operator connected the ASI2600MM Pro P25 via USB 2.
The selected-product probe confirmed `03c3:260e`, `bcdUSB 0210`, 512-byte bulk IN
`81`, configuration length 25 and Windows driver `01020200`. The existing
production capture implementation was tested without changing FPGA pacing.

`validate_asi2600_p25.py --long 30` passed all **40 cases / 42 frames**:
ten full-frame offset transitions checked every row across 32 us, 100 ms,
999,999 us, 1 s and 2 s; three fresh full frames; minimum/moved/edge ROI;
bins 1–4; gain boundaries; ROI exposures through 60 s; and full-frame 30 s.
All retained replays matched. A 12 MiB host-read interruption required two
retained retries (the first restart timed out); stopping the sender caused a
real bulk timeout and recovered after one retry. Both matched the interrupted
prefix. Abandoning a replay after 12 MiB also recovered with matching pixels.
The validator rejected no case. Long full-frame acquisition took 35.851 s.

A separate production pipe-worker test terminated an active 10-second exposure,
then reopened using the saved serial and locator. A full 52,183,296-byte frame
passed, followed by close/reopen and another distinct full frame. Both used
zero read retries, exact byte/digest checks and verified identity; no list or
discover protocol command was issued. All owned workers exited. No images
were saved/uploaded, installed settings changed, or USB ports cycled/reset.
See [sanitized summary](usb2-p25-evidence.json).

This establishes the tested P25 fallback on this Windows USB 2 setup. It does
not disprove the static output-pacing difference or validate a different
controller, camera revision, cold boot, physical disconnect or OS.

### Original ASI6200 follow-up hardware result

The operator next connected the original ASI6200MM Pro via USB 2. The selected
product probe reported `03c3:620b`, `bcdUSB 0210`, 512-byte bulk IN `81`,
configuration length 25 and Windows driver `01020200`. Every capture verified
revision 3 and HMAX 1515. No SDK or additional output-throttle change was used.

The stronger `validate_asi6200.py --revision 3 --full-controls --long 30`
accepted **63 cases / 65 frames** before stopping at case 63, full-frame gain
520. Accepted checks included three fresh full frames, bins 1–4, minimum/moved/
edge ROIs, ROI exposures through 60 s, and ten full-frame offset transitions
(200/50, 32 us through 2 s). The offset checks inspect every individual row,
not only the 16 broad bands. All accepted retained replays were identical.
A 12 MiB interrupted read recovered with two retries; deliberately stalling the
sender recovered from a real USB timeout with one. Both matched the original
prefix. Abandoning a replay after 12 MiB recovered identical pixels too.

**The matrix is not an unconditional pass.** Case 63's row-median extrema were
1614–1858 ADU and failed the new conservative per-row uniformity threshold.
Follow-up full captures at gains 520 (twice), 521 and 700 also failed that
threshold, despite complete byte/digest checks and identical retained replay
with zero retries. Their broad-band medians were much tighter. Previously saved
[USB 3 evidence](asi6200-original-evidence.json), native cases 63–65, has the
same effect: row ranges 1590–1856, 1688–1948 and 0–784 respectively. Thus this
is not evidence of a newly USB 2-specific failure. High-gain noise/clipping is
a plausible explanation, not a proven diagnosis. The strict gate remains;
do not silently relax it or relabel the failed samples as freshness passes.
Future validator failures retain sanitized statistics before exiting.

Returning to gain 100 passed the row check (1999–2002 ADU at offset 200).
A separate full 30-second exposure delivered 122,342,976 bytes in 40.446 s,
with zero retries and identical replay. The original matrix never reached that
case: this was an explicitly separate follow-up, not a successful matrix rerun.
See [sanitized USB 2 evidence](usb2-6200-original-evidence.json).

Production pipe-worker tests also passed: terminate an active 10-second
exposure, reopen with cached serial/locator, and download a fresh full frame;
reject a deliberately insufficient 5 ms transfer budget with a structured
timeout and zero image bytes, then accept a new full exposure; finally close/
reopen and capture again. The three successful frame hashes were distinct,
with zero retries and acquisition times 8.757, 8.869 and 8.778 s. No `list` or
`discover` protocol command was issued. All owned workers exited. No USB reset,
installed settings change, image save or upload occurred. Worker termination
does not substitute for physical disconnect testing.

### ASI6200 P25 follow-up hardware result

The operator then connected the ASI6200MM Pro P25 through USB 2. The selected
probe reported `03c3:620b`, `bcdUSB 0210`, bulk IN `81` with 512-byte packets,
configuration length 25 and Windows driver `01020200`. A small capture and
every matrix frame verified revision 5 / HMAX 880, independently of the
original edition's results. No SDK or output-throttle change was used.

The strict matrix accepted **42 cases / 44 frames** before stopping at case 42
(999,999 us, gain 100, offset 200). Accepted checks included three distinct
full frames with identical replay, bins 1–4, minimum/moved/edge ROI, the ROI
gain sweep through 700, ROI exposures through 60 s, and every-row full-frame
offset transitions at 32 us and 100 ms. Full 122,342,976-byte reads recovered
after a 12 MiB host interruption (two retries) and a real stalled-sender timeout
(one retry), with matching prefixes and replay. An abandoned 12 MiB replay
also recovered identical pixels.

**The initial dark-offset gate did not pass.** At 999,999 us the row medians were
2055–2071 ADU rather than within 15 ADU of 2000. The entire frame was elevated,
not separated into old/new offset bands. At 32 us, the offset-200 row range was
2002–2004; at 100 ms it was 2007–2010. Incoming light or an exposure-dependent
sensor pedestal may explain the increase, but neither is established. The
operator confirmed it was not fully dark-capped, then secured the cap and
noted that the sensor was warm from being outdoors. No threshold was relaxed,
and the failed frame's statistics are retained. With the cap secured, three
fresh full frames and the formerly failing 999,999 us case passed (2004–2006
ADU at offset 200). This supports stray light as the initial failure's cause,
without establishing a calibrated dark-current model. The first matrix did
not reach the remaining full-frame offset/gain cases; the capped follow-up
is separate, not a retroactive pass of the initial run.

The capped follow-up accepted **28 cases / 30 frames**: three new full frames,
all ten every-row offset transitions through 2 s, and full-frame gains through
521. At gain 700, case 28 stopped with row-median extrema 2768–3808 ADU,
exceeding the conservative uniformity limit. Byte counts, digests and retained
replay passed with no read retries, but that does not prove high-gain freshness.
The prior USB 3 P25 summary recorded only broad-band extrema (3232–3264 ADU at
gain 700), not every-row extrema, so it cannot resolve this stricter gate.
Neither warming nor high-gain noise should be asserted as the cause without
further evidence; no sensor cooling/settings change was made to mask it.

Two further gain-700 frames reproduced the row-range failure (2816–3840 and
2768–3808), while their broad-band ranges were only 3280–3328 and 3296–3328.
All retained pixels matched with zero retries. Returning to gain 100 restored
the expected short-frame row range, 2002–2004 at offset 200. A separate capped
30-second full frame then passed exact length/digest and replay with zero
retries, acquisition 39.981 s. Its row range was 2052–2056 at offset 200; the
short-exposure absolute-dark-offset gate is not applied to this long frame.
Warm-sensor dark current may contribute, but was not measured independently.

Production pipe-worker recovery passed independently of that dark-scene
assumption: termination during a 10-second exposure, cached-identity reopen,
5 ms transfer-budget rejection with a structured timeout and zero image bytes,
a subsequent exposure, and close/reopen. All three successful full frames had
different hashes, zero retries and acquisition times 8.105, 8.056 and 8.076 s.
No `list`/`discover` protocol commands were issued. No images were saved or
uploaded, installed settings changed, or USB port reset/cycle performed.
All owned test workers exited, including the final capped long exposure.
See [sanitized evidence](usb2-6200-p25-evidence.json). This is partial USB 2
hardware evidence, not an unconditional P25 matrix sign-off.

### Duo follow-up: two acquisition fixes

The operator connected the ASI2600MM Duo main sensor via USB 2. The selected
probe reported `03c3:2601`, `bcdUSB 0210`, bulk IN `81` / 512-byte packets,
25-byte configuration and Windows driver `01020200`. The guide sensor was
never opened. A 512×256 read/replay passed, but the stronger full-frame checks
exposed two failures missed by the earlier matrix:

1. At 32 us, gain 100 and offset 240, upper row bands reached 2400 ADU while
   lower bands retained near-saturated data, with row medians up to 65535.
   Changing to offset 50 and back to 240 changed only the upper bands. All
   retained replays matched, demonstrating why replay equality alone is not
   proof of a fresh sensor frame. Original Duo used a fixed 100 ms wait after
   the readout flag; its short full sensor interval is about 164.525 ms.
   The corrected guard uses the full programmed interval plus 100 ms, with
   revision-specific HMAX (779 for Duo, 790 for P25). This fixes sensor timing
   on both links, not just USB 2.
2. With the readout guard fixed, a 64×64 USB 2 retained read exhausted its
   retries with no bytes received. The Duo USB 2 path now shares the P25's
   minimum 128 KiB physical read, factory correction and exact ROI crop.
   The initial fix left original Duo USB 3 geometry unchanged; the subsequent
   USB 3 regression reproduced the same zero-byte timeout at 64×64 / 100 ms.
   The minimum now applies on both links. 64×64 origin, moved and edge
   requests then returned exactly 8,192 bytes with identical replay.

These are scoped fixes, not a relaxed freshness/timeout gate or a USB port
reset. The earlier failures remain rejected. The manual matrix can be
reproduced with `validate_asi2600_p25.py --duo --long 30` and a new output path.

The final build passed all **40 cases / 42 frames**, including ten every-row
full-frame offset transitions at 32 us, 100 ms, 999,999 us, 1 s and 2 s;
three distinct full frames; bins 1–4; minimum/moved/edge ROI; ROI gain boundaries
from -25 through 700; ROI exposure through 60 s; and full-frame 30 s.
Every retained replay matched. A 12 MiB interrupted read and a real stalled-
sender timeout each recovered after one retained retry, with matching prefixes.
Abandoned replay also recovered identical pixels. The 30-second full frame
took 35.808 s, used zero retries and delivered 52,183,296 bytes. These checks
do not characterize every-row noise at extreme gain; full-row offset gates
use gain 100, and the gain sweep uses a smaller ROI.

Production worker termination during a 10-second exposure followed by cached-
identity reopen passed. An intentionally insufficient 5 ms transfer budget
returned a structured timeout and zero image bytes, followed by a successful
fresh exposure and close/reopen capture. The three successful full frames
were distinct, used zero retries and took 4.880, 4.883 and 4.856 s respectively.
No `list`/`discover` protocol commands were issued. All test workers closed.
No guide camera, port reset/cycle, installed settings or image uploads were
involved. See [sanitized evidence](usb2-duo-evidence.json).

Local validation passed all workspace tests (50 camera-driver unit tests),
strict workspace clippy, fmt, the pipe simulators/synthetic SDK ABI suite and
11 pure research tests. The USB 3 regression below covers the changed Duo
readout guard. Other models' earlier evidence applies to their
tested builds; no blanket hardware sign-off is implied by these fixes.

### Duo USB 3 regression and tiny-frame follow-up

After the operator reconnected the same main camera via USB 3, the selected
probe confirmed PID `2601`, `bcdUSB 0300`, endpoint `81` with 1024-byte packets
and SuperSpeed burst 15. The first run passed all ten full-frame offset
transitions and three repeated full frames, but failed the 64×64 / 100 ms
retained read: zero of 8192 bytes arrived and the two-retry budget exhausted.
Thus the small-frame issue was not confined to USB 2. The read floor now
applies on both links; output remains exactly the requested ROI after factory
correction and cropping. No timeout or freshness gate was relaxed.

Source `506e562` passed the complete **40 cases / 42 frames** USB 3 matrix,
plus separate 64×64 origin/moved/edge captures (8192 output bytes from 131072
physical bytes, identical replay, zero retries). The full-row offset checks,
bins 1–4, ROI gains -25 through 700, ROI exposures through 60 seconds and
30-second full frame passed. The latter took 33.791 seconds, with zero retries.
Both the 12 MiB interrupted read and real stalled-sender timeout recovered
matching retained pixels after one retry; abandoned replay also recovered.

Production worker termination during a 10-second exposure, cached-identity
reopen, an exhausted 5 ms transfer budget with zero pixels, subsequent fresh
exposure and close/reopen passed. Three full frames had distinct digests,
zero retries and acquisition times 3.813, 3.789 and 3.807 seconds. No list or
discovery commands, guide opens, SDK loading, port resets, installation or
configuration changes occurred. All owned workers closed; no images were
saved or uploaded. See [sanitized USB 3 evidence](usb3-duo-regression-evidence.json).

The previous USB 2 evidence remains tied to its recorded source/binary; this
follow-up leaves USB 2 padding behavior unchanged, but the new binary has not
been rerun on USB 2. All workspace tests, strict clippy/fmt, pipe/synthetic-SDK
tests and 11 pure research tests passed. This does not settle the ASI6200
high-gain qualifications or establish cold-start, physical bus-fault, other
controller or non-Windows behavior.

## Finding: link-specific FPGA output pacing is missing

Static analysis used the repository's unmodified Linux x86-64
`libASICamera2.so`, SDK 1.41, SHA-256
`d1de4a5ab85c8cafbddfad9c593bbba515890d3adf20c1ca44dafcf15f2775ce`.
It was parsed as data, never loaded/executed. Symbols, branch addresses and
constants below can be reproduced with `scripts/inspection/inspect_usb2_sdk.py`.
The script refuses a different binary digest.

1. `CCameraBase::OpenCamera` at `0x29fc10` calls `IsUSB3Host`, then stores its
   boolean at object offset `0x12c` (`0x29fced` / `0x29fcf4`).
2. `CCameraFX3::IsUSB3Host` at `0x2ada40` reads vendor request `B4` and compares
   the returned byte with 1. This is separate from a USB descriptor revision.
3. `SetEnableDDR` stores the DDR-enabled boolean at offset `0x27e` (e.g. Duo
   `0x1ac2c2`). With DDR enabled, the following `SetFPSPerc` methods branch on
   the verified USB-host flag and select a different bandwidth multiplier:

   | SDK function | Start | USB 2 multiplier | USB 3 multiplier |
   | --- | --- | --- | --- |
   | ASI662MC SetFPSPerc | `0x13cd50` | 43,272 (data at `0x56b540`) | 400,000 |
   | ASI676MC_DDR SetFPSPerc | `0x1c8830` | 43,272 | 400,000 |
   | ASI2600MM_Pro SetFPSPerc | `0x1a6e30` | 43,272 | 395,000 |
   | ASI2600MM_Duo SetFPSPerc | `0x1ae450` | 43,272 | 395,000 |
   | ASI6200MM_Pro SetFPSPerc | `0x191ee0` | 43,272 | 390,000 |

4. The multiplier times the bandwidth control is divided by 400,000, then
   passed to `CCameraFX3::SetFPGABandWidth` (`0x2af1b0`). This computes
   `truncate(25600 / bandwidth - 256)`, bounds the result, and writes the
   little-endian divider through FPGA registers `24` and `25` under hold `01`.
   Single-precision evaluation at bandwidth control 40 gives USB 2 divider
   **5660 / `0x161c`**, versus USB 3 values 384, 392 or 400 for those multipliers.

Regain currently retains fixed output dividers: Bayer 662 = 384, Bayer 676 = 1,
2600 original = 3, 2600 P25 = 400, and 6200 = 400. These are not all equivalent
to the SDK's DDR-enabled USB 2 branch. The sensor frame/shutter padding added
for the 676 is a separate mechanism from this FPGA output throttle.

**Inference, not a measured failure:** slower FPGA output may matter on other
firmware/controllers or congested USB 2 buses despite the successful 662/676
tests. This identifies a concrete prototype candidate; it does not establish
that a universal `0x161c` write is safe or sufficient for every model. The SDK
also has non-DDR paths and model/revision-specific HMAX. No blanket sensor
register changes or extra vendor queries were enabled by this spike.

## Host transport and test audit

- Production open and all four acquisition families use the shared USB 2 gate.
  Camera selection/serial verification still precede sensor/environment writes.
- New inventory-driven unit coverage traverses `Model::ALL` using raw USB 2
  descriptors and the production parser/gate. Minimum/full-frame geometry and
  short/long-mode boundary validation run for every advertised identity. The
  guide's existing zero-line rejection and lack of retained replay stay explicit.
- The pipe simulator **does not execute the sensor acquisition paths**: it
  generates synthetic pixels when no device is present. Its recovery tests
  establish protocol/supervisor behavior, not USB register sequences, packet
  transfer, sensor freshness or firmware retention.
- Native transport rounds receive capacity to endpoint packet size while
  requiring the exact actual frame length. Windows delegates packetization to
  its existing driver; the USB 2 Bayer tests covered a non-aligned final chunk.
- Full 6200 RAW16 is 122,342,976 bytes, below the shared 128 MiB frame bound.
  The default 60-second whole-transfer budget needs over 2.04 MB/s on average,
  before control/processing overhead. A later chunk has a 5-second request
  deadline; increasing the whole-read budget alone does not change that.
  Slow/shared-bus failures must remain bounded and explicit, never partial
  frame success. Exposure time is not a substitute for a transfer budget.
- Research-only retained verification and port-operation CLI paths still have
  deliberate USB 3 restrictions; these are not production acquisition gates.
  No discovery polling, retry loops, automatic reset or SDK fallback was added.

## Next experiment / acceptance gate

1. Compare SDK and direct register `24/25` programming on the same owned USB 2
   662/676 at fixed settings, including DDR mode. Keep traces/private identities
   local. Test the link-specific output throttle independently of the existing
   sensor pacing; preserve the USB 3 path.
2. Repeat on each available 2600/6200 revision. On a capped camera alternate
   offsets and check **every row** for freshness, not just valid envelopes or
   identical replay. Replaying stale DDR can be byte-identical and still wrong.
3. Exercise full RAW16, supported bins, minimum/moved/edge ROIs, minimum valid
   exposure, 100 ms, 999,999 us, 1 s and 30 s. Repeat frames; verify exact lengths,
   changed frames, retained prefix/full replay where supported, cancellation,
   close/reopen without an inventory sweep, and bounded transfer exhaustion.
4. Repeat USB 3 cases as regression checks. Record negotiated endpoint layout,
   driver/firmware revision, exposure/read budgets, elapsed time and hashes,
   never private images/serials/calibration. No port cycling as a routine test.
5. Physical disconnect/reset, cold power-up and each OS require separate
   operator-authorized tests. SDK-only models require a separate bring-up, not
   an expanded PID allowlist. Only mark each matrix row verified after results.

The spike is complete as an audit. Universal working USB 2 support is **not**
proven and must not be a release claim. The remaining sensor hardware and
SDK/direct comparison gates above are still open.
