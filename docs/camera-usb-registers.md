# Direct camera USB register reference

This describes the writes in `regain-zwo::asi::direct`, based on SDK 1.41
traces, inspected SDK code and tests on the supported camera revisions.
It is an operational map, not a vendor register specification. A value can
be necessary to reproduce the observed sequence while its electrical purpose
remains unknown. The SDK backend continues to use vendor APIs.

Inline comments explain the write groups where they are used. This reference
connects the notation, shared registers, model differences and source evidence.

## Reading a write

`Camera::vendor(request, value, index, length)` uses USB setup-packet names:

| Argument | USB field | Meaning in the register protocols |
| --- | --- | --- |
| `request` | `bRequest` | Command, such as `0xbd` for an FPGA write |
| `value` | `wValue` | Register address for `BC`, `BD` and `B6`; immediate DAC code for `B2` |
| `index` | `wIndex` | Written register data for `BD`/`B6`; command-specific selector for other requests |
| `length` | `wLength` | Zero for OUT writes, nonzero for IN reads |

The transport selects vendor/device OUT `0x40` or IN `0xc0` from the length.
For example, `vendor(0xbd, 0x19, flags & !1, 0)` means **clear bit 0 of FPGA
register `19`, preserving its other bits**. It does not write register `BD19`
as one 16-bit address; `BD:19` is shorthand for request and address.
The Windows driver IOCTL numbers and 38-byte transfer header are a separate
host ABI in `protocol.rs`, not values written to camera registers.

| Request | Observed use | Important limit |
| --- | --- | --- |
| `BC` / `BD` | Read / write an FPGA register | Shared flag registers require read-modify-write |
| `B6` | Write sensor-side registers | Addresses and state values differ by sensor family |
| `A9` | Arm acquisition / start the selected stream | Cleanup and retained-frame replay must not issue another `A9` |
| `AA` | Full stop and discard retained-frame state | Clearing DDR does not by itself restore gated clocks |
| `AF` | Part of the traced initialization sequence | Its complete side effects are undocumented; initialization can reset thermal state |
| `BE` | Enter (`wValue=0`) / leave (`wValue=1`) the calibration-read sequence | Distinct from exposure arm/stop |
| `C3` | Read factory correction data in model-specific pages | Data belongs to the camera; tables do not embed it |
| `C8` | Read the camera's eight-byte serial | Used to preserve identity across reconnects |
| `B3` | Read two-byte signed little-endian Q8.8 temperature | Divide by 256 for °C; host control 8 separately uses tenths of a degree |
| `B2` | Send ASI585 cooler DAC demand in `wValue` | No traced DAC readback; not a `BD` register write |

Multi-byte fields are packed least-significant byte first by the model's
`word()` helper. Addresses ascend on ASI2600/6200 and Bayer profiles; ASI220
uses descending addresses. The ASI2600/6200 FPGA helper brackets multi-byte writes
with `BD:01=1` then `BD:01=0`, matching the SDK update sequence. The Bayer
sensor helper uses its own `B6:3001` bracket. These operational labels do not
establish that the two sensors implement the same internal latch.

## Shared FPGA flags and acquisition state

These meanings are supported by the traced acquisition/control paths. They
are not permission to reuse a model's whole register preset on another camera.

| Register / mask | Observed role | Write constraint |
| --- | --- | --- |
| `00 / 0x10` | Control edge used in synchronization and full-stop sequencing | Complete both synchronization edges; mask `0x10` means bit **4**, not bit 10 |
| `0b / 0x01` | Integration hold in the long-exposure paths | Clear during clock restoration before acknowledging cancellation |
| `0b / 0x10` | Additional long-exposure clock hold | Clear before `0x01`; preserve other bits with fresh reads |
| `19 / 0x01` | Low-power clock gate during integration | Clear before sensor/remaining clock restoration |
| `19 / 0x40` | Enable the dew heater, where advertised | Also set its output register; preserve the exposure bit |
| `19 / 0x80` | **Disable** cooling | Inverted polarity: clear enables cooling |
| `18` | Retained-read sender/cursor control | Stop sender with zero, reset/drain as the model requires, then write one to replay |
| `23` | Acquisition/retention status | `0x01` is observed full-stop idle; ASI2600/6200 ready/readout is `0x15`, Bayer retained-ready is `0x05`; neither proves every row is stored |
| `26` | ASI2600/6200 cooler output code | Use the SDK conversion below; zero-percent demand is not register zero |
| `2a` | Heater output preset | `0xc5` (197) on, zero off; not a percentage or wattage |
| `fa` / `fb` | Advertised fan / LED command | Raw 0…255, checked by register readback; not measured RPM or light output |

`BC:23=0x15` can precede completion of sensor readout. The model-specific
readout guard waits the programmed frame/line interval before freezing DDR.
Matching image boundary markers and counters only establish framing;
[fresh-row tests](asi6200-abort-cleanup.md#validation-and-hardware-acceptance)
are needed to exclude stale pixels.

## Clock sequences and timing constants

ASI2600 and ASI6200 share `host_timed.rs`, with explicit protocol profiles:

| Profile | Sensor standby register | >1000 ms compensation | 1000 ms compensation |
| --- | --- | --- | --- |
| ASI2600 original/P25 | `B6:01ee` | 400 ms | 205 ms |
| ASI6200 original/P25 | `B6:019e` | 200 ms | 200 ms |

The compensation numbers reproduce the inspected SDK timer arithmetic. They
are not measured USB latency and should not be tuned as retry backoff.
This branch uses the exposure truncated to whole milliseconds: 1,000,000
through 1,000,999 µs all select the 1000 ms compensation.
The synchronization loop allows five attempts, each a complete 5 ms high /
20 ms low pulse on `BD:00 / 0x10`. Cancellation is checked after the bounded
handshake, never between pulse edges.

During a long integration, the 100 ms service loop stages sensor standby,
`19 / 0x01` and `0b / 0x10` at iterations 6, 8 and 10. Those are nominal
600/800/1000 ms stages; USB calls and scheduling can delay their actual times.
Every exit restores the clocks in SDK order:

1. Clear `19 / 0x01`, preserving the thermal flags; wait 100 ms.
2. Write the profile's sensor standby register to one; wait 100 ms.
3. Clear `0b / 0x10`, then `0b / 0x01`, with fresh reads.
4. Read back both FPGA gate registers before reporting success.

Bayer still/video paths use `bayer/clocks.rs`: clear `19 / 0x01`, wait 200 ms,
then clear `0b / 0x10` and `0b / 0x01` and verify. Their sensor standby is
`B6:3000=1`, rather than either ASI2600/6200 register. Both families attempt
ordinary stop/`AA`/pipe cleanup even if restoration fails, and cannot report
a clean cancellation after an uncertain cleanup.

HMAX values are also model/revision-specific SDK timing inputs:

| Camera | HMAX used by the direct timing formulas |
| --- | --- |
| ASI2600 original / P25 | 779 / 790 |
| ASI6200 original / P25 | 1515 / 880 |
| ASI585MM Pro | 192 |
| ASI662MC | 230 |
| ASI676MC | 176 |

The formulas use the traced 20 MHz timing basis (`HMAX / 20` µs per line).
ROI padding, blanking rows, frame/shutter encodings and gain thresholds stay
in model profiles and their documented write groups; they are not universal
USB settings. See [Duo](duo-capture.md), [ASI2600 P25](asi2600-p25.md),
[ASI6200 original](asi6200-original.md), [ASI6200 P25](asi6200-p25.md),
[ASI585](asi585mm-pro.md), and [ASI662](asi662mc.md).

## USB output pacing and host read size

FPGA `24/25` is a little-endian output divider, updated under `BD:01`.
The inspected SDK's DDR-enabled ASI662/676 path computes
`truncate(25600 / bandwidth - 256)`, where
`bandwidth = control * link_multiplier / 400000` and the multiplier is
43,272 on USB 2 or 400,000 on USB 3. At control 40, this gives `0x161c`
or `0x0180`, respectively; the shared video path writes those values.
This is camera-side pacing, distinct from the host's bulk-read chunk size.
Smaller reads do not select USB 2 signaling or program this divider.

The [USB 2 SDK inspection](usb2-coverage-spike.md#finding-link-specific-fpga-output-pacing-is-missing)
records the functions, bounds and pinned Linux SDK binary hash. Other model
paths have different multipliers and fixed presets; the formula is evidence
for a scoped implementation, not permission to replace every divider value.

## Cooler and heater numbers

`environment.rs` shares the SDK type-1 current/DAC lookup. Requested percent
is converted to nominal current using the SDK's 6.01 A endpoint, then
interpolated between its calibration points. DAC 255 is zero demand; DAC 40
is full demand. These are command-calibration values, not current measurements.

ASI585 sends the DAC code directly with `B2`. ASI2600/6200 convert it using
integer arithmetic `(272 - DAC) * 220 / 256` and write `BD:26`.
Observed 0%, 1%, and 100% commands become 14, 16, and 199. Preserve truncation
and operation order when changing this code. SDK dispatch `10ec60`, table
`2876e0` and conversion `10e870` are recorded in
[the thermal mapping](duo-capture.md#nina-backend-and-environment-controls).

The target temperature and PI regulator live on the host; no camera setpoint
register is written. Regulator gains, slew limits, two-second elapsed cap and
one-second feedback deadline are Regain policy, separate from SDK calibration.
Initialization reapplies acknowledged output and actuator flags without
resetting that feedback deadline. Heater capability is checked separately:
the verified ASI585MM Pro profile exposes no controllable heater, so Regain
does not send heater writes to it.

## Opaque setup tables and provenance

`*_tables.rs` contains ordered volatile writes, including sensor tuning values
whose individual meanings have not been established. Such values are
**trace-pinned presets**, not named gain/ROI/clock fields merely because their
addresses resemble another sensor's. Preserve ordering and repeated writes
until independent evidence supports a change. Comments around runtime writes
explain the fields that have been mapped.

Each extractor below requires its reviewed trace SHA-256 and restricts the
selected write ranges/commands. It is not a general trace-replay facility.

| Table | Reviewed extractor / evidence |
| --- | --- |
| ASI220 | [extract_asi220_tables.py](../scripts/inspection/extract_asi220_tables.py), `duo-guide-mapped-01` |
| ASI2600 original | [extract_asi2600_tables.py](../scripts/inspection/extract_asi2600_tables.py), `duo-main-mapped-01` |
| ASI2600 P25 | [extract_asi2600_p25_tables.py](../scripts/inspection/extract_asi2600_p25_tables.py) |
| ASI6200 | [extract_asi6200_tables.py](../scripts/inspection/extract_asi6200_tables.py), P25 baseline; original HMAX/settings selected at runtime |
| ASI585 | [extract_asi585_tables.py](../scripts/inspection/extract_asi585_tables.py), `asi585mm-full-baseline` |
| ASI662 | [extract_asi662_tables.py](../scripts/inspection/extract_asi662_tables.py), `asi662-full40-20260930` |
| ASI676 | [extract_asi676_tables.py](../scripts/inspection/extract_asi676_tables.py), `map-final-full` |

The SDK 1.41 DLL used for this mapping has SHA-256
`0c8778c3cce2012961b079e3c7d0d8348a8b3823939335d9e98148cb5d5dc34a`.
[Cancellation evidence](asi6200-abort-cleanup.md) identifies the inspected SDK
worker locations and distinguishes hardware acceptance from register-model
tests. Private traces, factory maps and SDK binaries are not embedded in this
reference. A command readback proves the accepted register state, not the
electrical behavior of an undocumented signal or actuator.
