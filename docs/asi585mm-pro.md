# SDK-free ASI585MM Pro

Current source supports the **ZWO ASI585MM Pro** (`03c3:585e`) through the
existing **Direct USB** choice in NINA, native ASCOM, and Alpaca. This is the
monochrome cooled camera; it does not establish support for ASI585MC variants.
The driver shares the `regain-zwo` crate and `regain-device` worker with other
ZWO cameras. Windows uses the installed ZWO USB driver without loading
`ASICamera2.dll`; Linux/macOS use the shared native USB transport.

## Choose an acquisition mode

| Use case | Support |
| --- | --- |
| Save a long exposure after a failed download | Still RAW16 with bounded retries of the same retained image, including after a partial host read |
| NINA, local ASCOM, or an Alpaca camera slot | Existing camera integrations, cooler setpoint, temperature, and cooler power |
| Live preview through the worker API | Explicit RAW16 `video` mode, 32 µs–30 s; no retained-image replay |
| Keep only the latest image while adjusting exposure | [Continuous acquisition](continuous-acquisition.md), live exposure/gain updates, settings generations, and optional transition frames |
| Other pixel formats | Not implemented in Regain; both camera backends currently capture RAW16 |

NINA, ASCOM, and Alpaca use still capture. Video and continuous modes are explicit
worker APIs; they are not new ASCOM camera methods. A failed live frame may be
replaced by a fresh frame under the shared video recovery policy, never described
as a re-read of the same exposure.

## Capture limits

- Mono 3840 × 2160, 2.9 µm pixels, 12-bit sensor represented in RAW16.
- Software bins 1–4: factory correction runs before SDK-equivalent averaging.
- Output ROI minimum 64 × 64, width multiple of 8, even height. Physical sensor
  origins align to 2 pixels horizontally and 4 vertically; the frontends use a
  conservative four-pixel origin alignment. ROI coordinates are in binned pixels.
- Still exposure 32 µs–2,000 s; gain 0–600; offset 0–300, default 3.
- USB bandwidth fixed to the traced setting 40. Short captures use a minimum
  sensor interval near 100 ms while preserving integration lines, allowing
  retained downloads. This adds latency and is not a maximum-frame-rate driver.
- No color white balance, flips, hardware bins, RAW8, or automatic exposure in
  direct mode. SDK mode also captures RAW16 in Regain.

For a research capture, run `regain-device zwo camera-direct --capture-585
--replay`. Add `--width 1920 --height 1080 --bin 2` for a binned still image, or
`--video --max-fps 2 --frames 6` for six video frames without replay.

## Cooling and heater capability

The camera exposes temperature, cooler target (−40…30°C), cooler enable, and
cooler power. SDK 1.41 exposes **no controllable anti-dew heater, fan-speed, or
LED control** for this unit. Direct mode omits those controls and rejects their
use, including heater writes during reconnect.

The shared bounded PI regulator runs on the USB owner thread, including during
exposure waits. Temperature feedback loss disables cooling. ASI585 uses vendor
command `b2` for the cooler DAC; ASI2600/6200 use their existing FPGA output.
The regulator and calibration interpolation are shared, but the output command
is model-specific. Reported power is the controller's commanded percentage,
not an independent electrical measurement.

A new direct connection initializes the DAC to zero demand and starts with the
current sensor temperature as its target. Saved application settings then
restore the desired target and enable state. There is no traced DAC readback;
Regain does not claim to recover another application's previous power demand.

## Protocol and validation

The SDK reference is Windows x64 1.41, SHA-256
`0c8778c3cce2012961b079e3c7d0d8348a8b3823939335d9e98148cb5d5dc34a`.
Tracing occurred inside an owned diagnostic worker. Raw traces, identifiers,
factory maps, and image data remain local; the published evidence contains only
sanitized aggregate results.

| Operation | Observed mapping |
| --- | --- |
| Sensor/FPGA access | Vendor `b6` sensor writes, `bc`/`bd` FPGA reads/writes |
| Gain | Conversion mode `3030`; gain `306c..306d`; threshold 200, below `gain / 3`, above `(gain − 150) / 3` |
| Offset | Sensor `30dc..30dd` |
| ROI | Origins `303c/3044`; sensor width rounded to 16, height rounded to 4 plus two lines; FPGA dimensions are the requested image |
| Timing | FPGA HMAX 192 at clock 20000: 9.6 µs per line and 60 blanking lines; shared host timing for exposures ≥1 s |
| Frame processing | Check boundary markers/sequence, replace two pixels at each end from one row inward, apply the camera's ASID mono defect map |
| Retention | Sensor standby after ready status preserves the frame; FPGA `18` resets the download cursor without another exposure |
| Cooling | Signed 8.8 temperature via `b3`; cooler-disable bit 7 of FPGA `19`; `b2` DAC 255…40 using the traced current calibration |

The reviewed extractor `scripts/inspection/extract_asi585_tables.py` accepts only
the pinned baseline trace and selected volatile writes. Initialization excludes
cooler reset commands, EEPROM writes, firmware, and private calibration data.
Both shared still and video paths use the same model profile and mono processing.

Same-frame SDK comparisons passed full frame, offset ROI, edge ROI, gain boundaries,
32 µs through 1.001 s, and bins 1–4: independent Rust correction and factory-map
decoding matched the SDK output byte for byte. Direct hardware checks passed
28 still frames, 21 video frames through 30 seconds, and continuous exposure/gain
transitions through 20 seconds. A separate **600-second full-frame exposure**
recovered an interrupted download and produced an identical retained re-read,
with no additional exposure; a fresh short capture afterward also passed.
Cooling remained active during a 60-second exposure, and binned video passed at
bins 1–4. See [validation evidence](asi585-validation.json) for the measurements.

Hardware validation uses Windows USB 3. USB 2, Linux/macOS hardware, cold startup,
physical USB reset, optical accuracy, and overnight streaming are not established
by these tests. Direct support remains experimental.
