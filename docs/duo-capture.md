# ASI2600MM Pro Duo capture reverse engineering

In progress, 2026-09-14. The SDK remains the primary plugin backend. Main-sensor
and guide-sensor acquisition are separate implementations despite sharing the
camera enclosure and Windows driver package.

## Main-sensor pixel processing

SDK 1.41 x64 retrieval dispatches to RVA `14f130`. Its internal frame retrieval
returns at `14f1ed`; factory correction runs at `14f2fd` and completes at
`14f302`. The shared correction function is `1043c0`, also used by ASI676MC,
but this camera selects monochrome neighbor spacing 1 and 16-bit precision.
The first and last dwords are replaced from one row inward before correction.
Gamma 50 bypasses the lookup-table transformation. Software binning follows
correction: integer averages of 2×2, 3×3 or 4×4 pixels, without saturation.
Same-frame comparisons also match every byte at bins 2, 3 and 4, including
full physical coverage at bins 3 and 4. Flips remain to be validated.

Independent Rust ASID decoding and correction matched every SDK output byte in
three same-exposure comparisons: 512 × 256 at origin, full 6248 × 4176, and a
512 × 256 window at (16, 32). The full sensor map contained 26,091 defects;
the independently decoded index list also matched the SDK exactly. Raw pixels
and calibration remain in memory only. See [reviewed evidence](duo-capture-evidence.json).

The main SDK aligns horizontal ROI origins to 16 pixels and vertical origins to
two pixels. Sensor writes use vendor `B6`; FPGA registers use `BC`/`BD`. These
transport similarities do not make the ASI676 register initialization reusable.

## SDK-free main acquisition

`zwogain-direct --capture-duo` now performs its own initialization, ASID reads,
ROI, timing, gain/offset, exposure, RAW16 transfer, correction and software
binning. This is a research CLI; the NINA direct backend remains gated to the
previously validated ASI676MC. The SDK remains the default for every camera.
The reviewed volatile register table can be reproduced with
`scripts/inspection/extract_asi2600_tables.py` from its hash-pinned local trace.
No EEPROM/firmware writes or device-specific calibration are embedded.

The main sensor uses B6 registers `a8/a9` for horizontal origin in 16-pixel
units, `08/09` for vertical origin plus 25, `0a/0b` for physical height and
`1dd/1de` for physical width plus 24. FPGA `04/05` and `08/09` hold dimensions;
`40..43` holds the byte count divided by four. Bandwidth is currently fixed at
the observed setting 40: HMAX 779, clock 20 MHz, 48 blanking lines.
Sensor gain switches conversion mode at gain 100 and adds digital stages above
460; offset is multiplied by ten. Timing uses FPGA frame length and sensor
`18/19` shutter length; exposures at least one second use host timing.
The long worker at `14c30c..14c636` synchronizes FPGA `23` bit `10` by pulsing
the stop gate, then raises `0b` bit 0. Above one second it pauses sensor `1ee`
after six 100 ms iterations, raises `19` bit 0 at iteration eight, and raises
`0b` bit `10` at iteration ten. Completion clears `19` bit 0, waits 100 ms,
restores sensor `1ee=1`, waits 100 ms, and clears the two `0b` bits. This
sequence is now implemented; the initial simpler host pulse was incomplete.

The acquisition matrix covers 64×64 through 6248×4176, moved ROI, 32 µs,
999,999 µs, 1 s, 2 s and 30 s, gains −25 through 700, offsets 0 through 240,
bins 1–4, and repeated fresh captures. See the sanitized evidence JSON and
`scripts/inspection/validate_duo.py`. Pixel buffers stay in memory and IPC
digests are checked independently.

### Retained-frame reads and current limits

FPGA `23` transitions from 1 after full stop to `15` during capture. Immediate
sensor standby at this transition can leave a small ROI unreadable; an
empirical 100 ms guard resolved the tested 64×64 case. Sensor standby uses
`1ee=5`, `00=5`, **without AA or the FPGA full-stop bit**, which clear retained
state on this unit. The exact firmware completion edge is not yet understood.

Short integrations stream: the driver explicitly reads frozen DDR after
standby. Long integrations already schedule an initial USB pass, which must
be consumed before asking for replay. Prematurely issuing `18=1` caused a
repeatable first-read timeout. Selecting the correct initial read path removed
that timeout at 1, 2, 5 and 30 seconds, with identical retained pixel replay.

Retained read/retry drains and resets endpoint 81, writes FPGA `18=0`, waits 100 ms,
checks idle and retention, then writes `18=1`. RAW16 envelopes contain `7e5a`
and `f03c` boundary bytes and matching 16-bit counters. These counters can be
zero and advance between retained reads; they are not a global freshness ID.
Replay comparison excludes the two envelope dwords, whose counters change.

Full-frame replay matched every pixel. After deliberately abandoning a read
at 12 MiB, the next read matched that prefix and a subsequent complete replay.
This retries transfer of retained pixels without another exposure command.
The final matrix contains 18 frames in 13 cases; normal captures used zero
read retries. The deliberate interrupted read used one. Earlier failing
experiments and the old matrix remain in evidence/history. Physical USB removal,
power loss and persistence across reconnects have not been tested.

## Guide processing

The guide retrieval target is RVA `d2590`. Before defect correction it unpacks
each wire word as `((low_byte << 4) | (high_byte & 15)) << 4`. At gain below 100,
the inspected SDK additionally uses a pseudorandom choice to toggle the decoded
sample's lowest bit for samples above 31. This branch must be accounted for in
same-frame comparisons; two independent random streams cannot be expected to
produce byte-identical output. SDK RNG routines `210dfc/210e14` use a 32-bit
LCG (`state = state*214013+2531011`, output bits 16..30), seeded with
GetTickCount at `d26a3`. Only eligible samples consume random values.

Rust now matches every output byte with the observed seed: gain 100 at
512×256, gain 0 at full 1920×1080 and gain 0 with full-sensor bin 2. Independent
ASID decoding matches all 4,147 guide defects. Guide correction uses spacing 1
and 12-bit precision, followed by software averaging. The guide advertises bins
1 and 2; bin 4 was rejected before capture by the SDK capability check.

## Remaining work

Refine the precise main-frame completion edge, expand held-out gain/timing
register comparisons and characterize cooler regulation across ambient conditions.
RAW8/video, flips and hardware binning need separate validation. P25 revisions
and physical cold-power/USB-fault behavior require separate hardware evidence.

## SDK-free guide acquisition

`zwogain-direct --capture-guide` implements the guide's own volatile sensor
initialization, calibration reads, physical ROI, RAW16 format, gain, offset,
timing, streaming transfer, unpacking/dither, factory correction and bin 1/2.
Its reviewed 265 sensor writes match both trace transactions and the SDK's
initialization array at `28d360`. Reproduce them with
`scripts/inspection/extract_asi220_tables.py`.

The guide selects RAW16 with vendor `AC`, and vendor `B5` carries
`ceil(wireBytes/49152)`, split across the two setup words. The four crop limits
use sensor `3200..3207`, output dimensions use `3208..320b`, line timing uses
`320c/320d`, frame length uses `320e/320f`, and integration uses `3e00..3e02`.
`AF`, `A9`, and sensor `0100=1` start capture; sensor `0100=0` and `AA` stop it.
USB2 packets and `11aa00bb` / `bb00aa11` frame boundaries differ from the main
sensor. No main-sensor DDR commands are sent to the guide.

The worker accepts only complete, correctly bounded frames. A startup transfer
can be partial or have old geometry, so it discards up to two unsuccessful
passes and resynchronizes to the next streaming frame. **This is a new sensor
frame, not retained-frame replay.** There is no proof of replay on the guide.

The final guide matrix contains 12 valid frames: full resolution, moved ROI,
bin 2, gains 0/100/349/350/400/600, offsets 200/400/1500, and exposures from
100 ms through 10 seconds. Digests differ across repeated captures and match
the binary IPC payloads. A 64×64, 32 µs capture also failed in the SDK;
zero-line integrations are explicitly rejected before hardware access until
their behavior is understood. The guide's advertised exposure limit is 10 s.
See `scripts/inspection/validate_guide.py` and the sanitized evidence JSON.


## NINA backend and environment controls

Both sensors now use these acquisition paths through the isolated plugin host.
The model catalogue gates exact PIDs/USB versions, verifies hardware serials,
and exposes main bins 1–4 and guide bins 1–2. Factory corrections are identical
to the research paths. The SDK remains the default, with a persisted opt-in
fallback that restores serial, imaging controls, cooler target/enable and dew.
A failed capture can switch only on a permitted retry; settings outside the
verified direct range route before exposure. SDK fallback stays active until
reconnect. The main and guide can therefore use longer SDK exposures without
extending unverified direct timing ranges.

Duo temperature is vendor IN `B3`, two signed little-endian bytes / 256 °C.
FPGA `19` bit `80` disables the cooler; bit `40` enables the dew heater.
Dew enable also writes `2A=197` (off: zero). Writes preserve unrelated bits,
including the exposure lifecycle bit. Target temperature exists in host state;
firmware accepts cooler output, not a temperature set point.

SDK cooler dispatch `10ec60` uses the observed type-1 calibration
`[40,255,6.01,0]`: requested power becomes current, then interpolates the
current/DAC table at `2876e0`. DAC endpoints 255/40 correspond to 0/6.01 A;
`10e870` converts DAC using `(272-DAC)*220/256` and writes FPGA `26`.
Observed 0%, 1%, and 100% outputs are 14, 16, and 199. Rust uses this mapping
with its own PI regulator, output ramp limited to two percentage points per
second, and bounded elapsed time after stalled I/O. Invalid temperature
feedback disables cooling and fails the worker. Idle polling, exposure waits
and transfer chunks service the regulator on the transport owner thread.
Sensor initialization preserves cooling/dew instead of resetting `19/26`.
Normal close disables the host-regulated cooler; process crashes leave the last
hardware output until recovery reconnects, as there is no established firmware
watchdog for host loss. Abrupt power/USB fault tests remain outstanding.

Hardware validation through the plugin protocol: twelve consecutive 5-second
512×256 main captures with cooler target 25 °C and dew enabled, zero recoveries;
temperature moved from 27.8 °C to 24.6 °C, with minimum 24.3 °C. Two guide
960×540 bin-2 captures also succeeded. Killing the direct main worker at download
caused one SDK retry after the 5-second reconnect delay, serial verification,
control restoration and three cooling samples near the prior temperature.
The following exposure stayed on the SDK with zero retries. Local logs retain
full diagnostic details; sanitized evidence omits camera identifiers.
