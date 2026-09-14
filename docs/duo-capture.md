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

Read/retry drains and resets endpoint 81, writes FPGA `18=0`, waits 100 ms,
checks idle and retention, then writes `18=1`. RAW16 envelopes contain `7e5a`
and `f03c` boundary bytes and matching 16-bit counters. These counters can be
zero and advance between retained reads; they are not a global freshness ID.
Replay comparison excludes the two envelope dwords, whose counters change.

Full-frame replay matched every pixel. After deliberately abandoning a read
at 12 MiB, the next read matched that prefix and a subsequent complete replay.
This retries transfer of retained pixels without another exposure command.
A 30-second full frame needed one automatic retained-read retry after its first
read timed out; extending the guard by a physical readout period did not fix
that behavior. The worker reports it explicitly. Physical USB removal,
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

Complete guide acquisition and its distinct USB2 lifecycle, refine main-frame
completion and long-exposure first-read behavior, verify gain/timing register
formulas against held-out traces, and add main cooling control/restoration.
RAW8/video, flips and hardware binning need separate validation. P25 revisions
and physical cold-power/USB-fault behavior require separate hardware evidence.
