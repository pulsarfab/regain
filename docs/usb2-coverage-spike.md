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
| ASI2600MM Duo / `2601` | Original main sensor | Descriptor/contract fixtures only for new fallback | USB 2 initialization, DDR freshness and complete capture/recovery matrix |
| ASI2600MM Pro / `260e` | P25 | Windows USB 2: 40 cases / 42 frames, all-row freshness, bins/ROI, retained recovery, production cancellation/reopen passed | SDK output-rate comparison, cold start, actual bus faults; do not reuse original timing |
| ASI6200MM Pro / `620b` | Original, BC:1c = 3 | Descriptor/contract fixtures only for new fallback | Same, including full 122,342,976-byte frames |
| ASI6200MM Pro / `620b` | P25, BC:1c = 5 | Same descriptor; distinct revision/timing | Independent full-frame freshness/recovery matrix |
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
