# SDK-free ASI676MC capture

The experimental `zwogain-direct` Rust executable now opens the installed
`ASICAMUSB3.sys` interface, initializes the sensor, configures a capture, waits
for a complete buffered frame, reads it, and returns RAW16 over a binary pipe.
It does not load or call `ASICamera2.dll`. The NINA camera uses the supervised
SDK host by default. The packaged `zwogain-direct.exe --serve` is available
through an explicit experimental setup option for the verified ASI676MC modes.

## Supported research configuration

- Observed USB3 ASI676MC only: VID `03c3`, PID `676d`, bulk-IN endpoint `81`.
- Bin 1, RGGB RAW16; even ROI origin, width multiple of 8, even height,
  minimum 64 × 64, maximum 3552 × 3552.
- Exposures 32 µs through 30 seconds, gain 0–600, offset 0–200.
- USB bandwidth configuration fixed at the traced limit of 40.
- Configurable retained-frame read retries, default 2, maximum 5.

Initialization uses reviewed volatile sensor/FPGA register transactions. It
does not replay firmware or EEPROM writes or embed camera calibration tables.
`extract_asi676_tables.py` reproduces the constants from the exact pinned local
trace. Exposure, ROI, gain and offset are explicitly set on each capture,
independent of the previous application's settings. Physical cold-power startup
has not been tested; the camera was already powered during these experiments.

The first/last two wire pixels contain transport words. Their magic values and
matching sequence are checked, then replaced from two rows inward, following
the observed SDK bin-1 operation. The reader now loads the camera's factory
calibration and applies the SDK-equivalent Bayer defect correction. Same-frame
comparisons against SDK 1.41 are byte-exact for the tested bin-1 RAW16 cases;
see [correction details and evidence](factory-defect-correction.md).
Binning, non-default gamma, flips, other correction-map types and cooling remain
unimplemented in the direct backend. Capture selects the ASI676MC interface specifically
when other ZWO models are attached, and refuses ambiguous duplicate models.

## Acquisition and retention

Observed ASI676MC register operations:

| Operation | Transaction / interpretation |
| --- | --- |
| Sensor register write | Vendor OUT `b6`, `wValue=register`, `wIndex=value` |
| FPGA register read/write | Vendor IN `bc` / OUT `bd` |
| Configure dimensions | Sensor `303c/3044` origin, `303e/3046` width/height; FPGA `04/08` dimensions, `40..43` frame byte count divided by four |
| Short exposure timing | FPGA `10..12` frame lines, sensor `3050..3052` shutter lines |
| Start | Observed `a9` acquisition start plus sensor `3000=0`; clear FPGA `00` bit `10` after startup delay |
| Long exposure | At ≥1 second, FPGA `00` bits `c0` select host timing; pulse FPGA `0b` bit 0 for the requested duration |
| Complete retained frame | FPGA `23` changes from `1` after full stop to `5` |
| Preserve buffered frame | Sensor standby `b6/3000=1`, without the full-stop `aa` command |
| Replay | FPGA `18=1` starts retained-frame transfer |
| Restart interrupted replay | Drain requests, abort/reset pipe, `18=0`, 100 ms settling delay, verify idle and retained status, then `18=1` |
| Full stop | FPGA `00` bit `10`, sensor standby, `aa`; observed to clear retained status from `5` to `1` |

These meanings are inferred from traces and direct experiments on this model,
not a vendor protocol specification. Readout uses bounded 1 MiB requests, checks
Windows/NT/USB status and exact byte counts, and requires matching frame
boundary words (`5a7e` / `3cf0`, little endian on the wire). The observed sequence
is 1 for each newly armed exposure, so it is **not** a global freshness ID.
Before arming, the old retained state must clear. The camera must then report
completion before the sensor is frozen and download begins.

For USB limit 40, the observed line period is `176 * 1000 / 20000 = 8.8 µs`.
Short-exposure frame/shutter calculations preserve the SDK's single-precision
rounding. At ≥1 second the hardware base interval depends on ROI height and
the requested duration is timed by the host. Gain changes conversion mode at
180; the register value is `gain/3` below that boundary and `(gain-78)/3` above
it. Version-pinned disassembly and traced register values anchor these formulas.

## Recovery experiments and limits

Complete SDK-free frame replay matched every byte at full resolution and at
512 × 256, for both 100 ms and one-second captures. Interrupting a replay after
3 MiB initially showed that writing `18=1` again while it was already active
does not restart transfer. Clearing the bit, allowing it to settle, then raising
it again restored replay.

A later 100 ms interrupted replay returned different bytes while the sensor
was still running. The diagnostic rejected that result. Sensor standby before
readout preserved the frame in follow-up tests. A full `aa` stop instead cleared
the retained-frame flag and is therefore reserved for final cleanup.

The hardware matrix tests interruption at several offsets and compares every
byte against an independently completed reference read from the same exposure.
An additional first-read interruption checks that recovery matches the original
prefix and a subsequent complete replay. These are deliberate host read
interruptions, **not injected USB bus faults or naturally occurring camera
failures**. Retention across cable removal, power loss or reconnect is not
established. The prototype never concatenates partial attempts and fails if the
camera no longer reports a retained frame. Exhausting read retries surfaces an
error. In plugin mode the existing C# supervisor then handles eligible fresh
exposure/process-restart recovery using the same selected backend. The standalone
`--capture` command does not perform that outer recovery.

## Plugin protocol and identity

`--serve` speaks the same version-1 framed JSON/binary protocol as the SDK host.
A dedicated worker opens and retains the exclusive camera handle; `start`
queues acquisition and `status` remains responsive while the worker operates.
`download` returns the complete, corrected frame. A single per-connection
watchdog bounds a stuck acquisition. NINA cancellation terminates the process;
an explicit `stop` during acquisition reports that process termination is needed.

Controls represent requested configuration and are staged until the next capture,
where the verified register sequence explicitly applies them. `get` reports that
configuration, not a new physical register read. Bin 1, exposure limits, fixed USB
bandwidth and unsupported telemetry are reflected in the advertised capabilities.
The server validates settings again before issuing hardware writes.

The SDK's `ASIGetSerialNumber` path at RVA `108800` uses vendor IN `C8`, value/index
zero, length eight. The direct implementation reads and formats those same bytes,
and a hardware comparison confirmed equality with the SDK serial. Zero serials,
missing identities and ambiguous initial selection fail; saved identity never
falls back to another model. This is independent of EEPROM defect-map decoding.

Very small 8 × 2 capture failed in both the direct prototype (invalid envelope)
and the SDK (exposure state 3). The research interface rejects ROIs smaller than
64 × 64 rather than advertising the entire theoretical SDK geometry range.

## P25 ASI2600 / ASI6200 transport

The installed ZWO driver INF, version 3.28.0.0, maps ASI2600MC/MM Pro IDs
`260a`/`260e` and ASI6200MC/MM Pro IDs `620a`/`620b` to the same `ASICAMUSB3`
service, driver binary and interface GUID as the ASI676MC:
`{c5b27530-3592-4e87-9e99-c2bafd5e5692}`. It does not label separate P25 entries.
This supports reusing the Windows transport for the P25 cameras using that
package; it does not establish identical sensor initialization or firmware
commands. Their actual descriptors and protocol traces still require hardware.

ZWO's [ASI6200 product page](https://us.zwoastro.com/products/asi6200) explicitly
lists P25 models with USB 3.0 and 512 MB DDR3. The official
[driver distribution](https://www.zwoastro.com/software/camera-driver/) uses a
shared camera-driver package. The direct backend deliberately refuses those
cameras until their model-specific acquisition and cooling are validated.

See [reproduction commands](../scripts/inspection/README.md#sdk-free-capture-and-replay)
and [hardware evidence](sdk-free-capture-evidence.json).
