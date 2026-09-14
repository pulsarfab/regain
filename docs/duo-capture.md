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
Gamma 50 bypasses the lookup-table transformation. Binning and flips follow
correction and require separate validation.

Independent Rust ASID decoding and correction matched every SDK output byte in
three same-exposure comparisons: 512 × 256 at origin, full 6248 × 4176, and a
512 × 256 window at (16, 32). The full sensor map contained 26,091 defects;
the independently decoded index list also matched the SDK exactly. Raw pixels
and calibration remain in memory only. See [reviewed evidence](duo-capture-evidence.json).

The main SDK aligns horizontal ROI origins to 16 pixels and vertical origins to
two pixels. Sensor writes use vendor `B6`; FPGA registers use `BC`/`BD`. These
transport similarities do not make the ASI676 register initialization reusable.

## Guide processing observations

The guide retrieval target is RVA `d2590`. Before defect correction it unpacks
each wire word as `((low_byte << 4) | (high_byte & 15)) << 4`. At gain below 100,
the inspected SDK additionally uses a pseudorandom choice to toggle the decoded
sample's lowest bit for samples above 31. This branch must be accounted for in
same-frame comparisons; two independent random streams cannot be expected to
produce byte-identical output. Hardware validation is still pending.

## Remaining work

Decode and independently test acquisition initialization, dimensions, timing,
gain/offset, binning, transfer completion and retained-frame replay on both
sensors. Add main-camera cooling control and verify restoration. P25 revisions
and physical cold-power/USB-fault behavior require separate hardware evidence.
