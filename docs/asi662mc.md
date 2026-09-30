# SDK-free ASI662MC

Current source builds support the **ASI662MC USB3** (`03c3:662b`) in NINA,
native ASCOM, and Alpaca through the existing **Direct USB** camera option.
The driver lives in `regain-zwo`; the shared `regain-device` worker owns the
connection. Windows uses the installed ZWO USB driver, without loading or
calling `ASICamera2.dll`. Linux and macOS use the existing native USB transport.

## Supported capture

- RGGB RAW16, bin 1, 1920 × 1080, 2.9 µm pixels, 12-bit sensor.
- ROI: minimum 64 × 64; width a multiple of 8, even height, origin aligned to
  8 pixels. Unsupported origins are rejected rather than silently adjusted.
- Exposure: 32 µs–2,000 s; gain 0–600; offset 0–300, default 15.
- USB bandwidth fixed at the traced value 40. No cooling, ST4, bin 2, other
  formats, flips, white balance, or automatic exposure controls in direct mode.
- Factory Bayer defect correction, complete-frame boundary checks, bounded USB
  transfers, and configurable rereads of the same retained frame.

Short exposures use a minimum sensor frame interval of approximately **100 ms**
to keep full-frame downloads replayable. Frame and shutter-delay registers are
extended equally, preserving the requested integration lines. This adds capture
latency; this backend is intended for individual recoverable images, not maximum
planetary video throughput. Exposures of one second and above keep the traced
host-timed sequence. Retrying a download does not take another exposure.

## What is shared

`asi662.rs` and `asi676.rs` supply separate sensor profiles and reviewed volatile
initialization tables to `bayer.rs`. They share acquisition, ASID calibration
reads, Bayer correction, frame validation, retained-frame retries, and cleanup.
They retain different gain addresses, conversion thresholds, sensor dimensions,
timing, and ROI rules. No new crate or executable is needed.

The supervisor supplies cancellation, same-serial reconnection, and bounded
replacement exposures consistently across NINA, ASCOM, and Alpaca. The ASI662MC
identity is also accepted by the opt-in USB recovery binder; physical USB reset
on this model has not been tested. Download replay does not require a USB reset.

## Protocol evidence

SDK 1.41 Windows x64 SHA-256:
`0c8778c3cce2012961b079e3c7d0d8348a8b3823939335d9e98148cb5d5dc34a`.
The installed USB driver reported `0x01020200`. The SDK was traced only inside
our own diagnostic worker; raw traces and calibration remain local.

| Operation | Observed implementation |
| --- | --- |
| Volatile sensor writes | Vendor `b6`; FPGA reads/writes `bc`/`bd` |
| Gain | Sensor `3030` conversion mode; `3070..3071` gain. Below 200: `gain / 3`; at 200+: `(gain - 150) / 3` |
| Offset | Sensor `30dc..30dd` |
| ROI | Sensor `303c/3044` origin; `303e/3046` dimensions. Sensor width and height round up to 16; height adds two lines. FPGA dimensions remain the requested image size |
| Timing | FPGA `10..12` frame lines, sensor `3050..3052` shutter lines; line period `230 × 1000 / 20000 = 11.5 µs`, 60 blanking lines |
| Retention | Clear old frame with full stop; require status `23 = 5`; sensor standby preserves the buffered image; `18 = 0`, idle wait, then `18 = 1` restarts reads |
| Pixel processing | Check `5a7e`/`3cf0` envelope and sequence; replace envelope pixels from two rows inward; apply the camera's ASID Bayer map |

Gain and exposure dispatch were corroborated by version-pinned disassembly at
RVAs `0x5dd00` and `0xa8c80`. The extractor
`scripts/inspection/extract_asi662_tables.py` accepts only the reviewed full-frame
trace hash and selected zero-length volatile writes. It excludes EEPROM writes,
firmware, identity, and calibration payloads.

The SDK matrix passed 45/46 cases through 60 seconds and restored the recorded
controls. Its one failed case requested origin `(16, 2)`, which the SDK adjusted;
aligned follow-up ROIs passed. Independent Rust processing matched SDK output
byte-for-byte for full frames and several ROIs, including non-16-aligned sizes.

Direct hardware tests verified full frames, aligned edge windows, gain transition
and offset limits, repeated fresh captures, 32 µs–60 s exposures, complete replay,
and interrupted reads followed by byte-identical replay. Zero read retries
surfaces the injected interruption. The initial unpaced short full-frame replay
timed out; the documented minimum frame interval resolved it in the test matrix.
These are deliberate host-read interruptions, not injected USB bus errors.
A physical Alpaca test captured a new frame, aborted a 60-second exposure in
16 ms, and captured again through a new worker with the same serial.

See [reviewed validation results](asi662-validation.json). Physical tests used
Windows and the already-powered camera. Cold startup, optical geometry/Bayer
orientation against a scene, illumination accuracy, physical disconnection,
Linux/macOS hardware, and 2,000-second captures remain unvalidated on this model.

## Reproduce

Disconnect other owners of this camera before running hardware tests:

```powershell
cargo build --workspace --locked
target/debug/regain-device.exe zwo camera-direct --capture-662 --replay
target/debug/regain-device.exe zwo camera-direct --capture-662 --microseconds 32 --interrupt-read-after-bytes 1048576 --replay
.reference/inspection-venv/Scripts/python.exe scripts/inspection/validate_asi662.py --long --output artifacts/inspection/asi662-new-validation.jsonl
```

`--capture-662` selects only the ASI662MC PID and requires exactly one matching
camera. Production connections use saved serials to distinguish multiple units.
The SDK remains available for its wider feature set.
