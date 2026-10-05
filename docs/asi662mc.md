# SDK-free ASI662MC

Regain supports the **ASI662MC** (`03c3:662b`) on USB 3 and
[USB 2 high-speed](usb2-cameras.md) in NINA,
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
  formats, flips, hardware white balance, or automatic exposure controls in direct mode.
- The pipe worker offers opt-in [shared software white balance and AWB](white-balance.md),
  with unscaled raw output by default; this is not a sensor register control.
- Factory Bayer defect correction, complete-frame boundary checks, bounded USB
  transfers, and configurable rereads of the same retained frame.

Short exposures use a minimum sensor frame interval of approximately **100 ms**
to keep full-frame downloads replayable. Frame and shutter-delay registers are
extended equally, preserving the requested integration lines. This adds capture
latency; this backend is intended for individual recoverable images, not maximum
planetary video throughput. Exposures of one second and above keep the traced
host-timed sequence. Retrying a download does not take another exposure.

<a id="experimental-continuous-video-source-builds"></a>

## Experimental video

The Direct USB pipe worker additionally accepts explicit `mode: "video"` for
the ASI662MC. Still capture remains the default; existing NINA, ASCOM, Alpaca
clients do not switch to video automatically. AutoPierCam 0.2.18 opts into this
mode when the selected camera advertises it and exposure is at most 30 s. This
video path supports RAW16/bin 1, the same ROI rules, and **32 µs–30 s** exposures.
The ASI676MC has its own [video validation and sensor profile](asi676-video.md);
other models retain their existing still-capture paths.

Repeat `start` → `status` → `download` for each requested frame. Matching settings
reuse the armed sensor and factory calibration. Exposure, gain or ROI changes
stop and reconfigure the stream; changing only `maxFps` does not. `stop`/`close`
cancel pending exposure and pacing waits. A stuck native transfer remains bounded
by the isolated worker's deadline; this mode does not reset a physical USB port.

`maxFps` defaults to 1 and accepts **0.01–120**, including 0.5 FPS. It caps grabs,
not merely publication: the next grab waits one interval after the previous
completed grab. Exposure/readout can lower actual throughput further. There are
no catch-up bursts. This is not a sensor frame-rate control: the sensor may
continue producing frames, gaps are reported, and buffered frames are not
guaranteed to be the newest scene after a slow consumer resumes.

Video never replays a retained exposure (`readRetries` must be omitted or zero).
Current source discards a complete transfer with bad boundary markers or mismatched
boundary sequence, stops/reinitializes the stream, and attempts **one fresh exposure**
inside the same request. It never delivers the rejected pixels or resets a USB port.
The replacement grab respects the FPS interval and cancellation. A second bad frame,
failed stop/reinitialization, incomplete transfer, or other hardware error remains
fatal; duplicate/backward video sequences are also fatal. Recovery logs
`video.frame_discarded` / `video.framing_recovered` and returns `framingRecoveries`.
This bounds a transient error; it does not establish the cause of reported USB 2
framing loss or guarantee long-running stability.
Shared software WB/AWB remains opt-in. `dark: true` is rejected;
physically cap the camera when collecting dark video.

`open` advertises `captureModes`, `videoMaxExposureMicroseconds`, and
`videoFrameRecoveryAttempts: 1`. Clients imposing their own frame deadline must
allow two exposures, two FPS intervals and stream-restart/transfer overhead. The
worker's default watchdog includes both attempts; an explicit capture deadline
remains authoritative. Example
`start` parameters for one requested video frame:

```json
{"mode":"video","maxFps":0.5,"width":1920,"height":1080,"bin":1,"x":0,"y":0,"microseconds":100000,"dark":false}
```

The research CLI also supports `--capture-662 --video --max-fps 0.5 --frames 3`.

For the reported gain-300, 6.4 s → 25 s USB 2 transition, the manual
`scripts/inspection/check_video.py --long-transition-only --output <local.jsonl>`
profile captures that transition twice plus a repeated 25 s frame. It requires
an idle, operator-authorized camera, validates lengths/checksums, and keeps only
metadata/aggregate brightness diagnostics, not images. `--worker` selects an
explicit comparison binary; `--simulate` never opens hardware.
`--stream` alone still means binary output framing, not video acquisition.

### Video evidence and limits

Owned SDK 1.41 Windows traces covered full-frame 100 ms, 1.6 s and 6.4 s capture,
plus a 512 × 256 ROI on USB 2. Video keeps the sensor armed between downloads,
unlike retained still capture. Bandwidth 40 uses FPGA word `0x161c` on USB 2
and `0x0180` on USB 3. The long-exposure low-power/XHS gates are corroborated by
the digest-pinned Linux SDK's ASI662MC `WorkingFunc` at `0x13b730`. Monotonic
deadlines implement the intended integration timing rather than reproducing
Windows polling-sleep rounding.

The explicit manual worker matrix passed on USB 2: 21 video frames covering
full-frame/ROI, 0.5 FPS, slow consumers, exposure changes through 30 s, cancellation,
restart and return to still capture. Long-exposure stop took about 218 ms; a
100-second FPS wait cancelled immediately at the measurement resolution. An
initial FPS test exposed early grabs after initialization; pacing now starts
from the previous completed grab and survives exposure changes. Pixels were
validated and discarded, not saved or uploaded.

The same 21-frame matrix subsequently passed with the ASI662MC on USB 3 while
an ASI676MC remained attached on USB 2. The 0.5 FPS frames arrived about
2.08–2.09 s apart; long-exposure stop took 203 ms and FPS-wait cancellation 16 ms.
Return to still capture passed. A separate three-frame SDK USB 3 reference
confirmed bandwidth word `0x0180`. The Direct worker never opened the 676;
the explicitly authorized SDK reference did perform its discovery pass.

These checks do not establish optical accuracy, cold startup, USB-disconnect
recovery, long-running stability or Linux/macOS hardware behavior for video.

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
and offset limits, repeated fresh captures, 32 µs–600 s exposures, complete replay,
and interrupted reads followed by byte-identical replay. Zero read retries
surfaces the injected interruption. The initial unpaced short full-frame replay
timed out; the documented minimum frame interval resolved it in the test matrix.
These are deliberate host-read interruptions, not injected USB bus errors.
Two full-frame 600-second captures completed acquisition in 600.129 s and
600.243 s. Both replayed byte-for-byte. The second recovered a download
interrupted after 1 MiB, then recovered a replay interrupted after 3 MiB.
The captures had distinct hashes, and a subsequent 1 ms capture passed.
These long frames were largely saturated under the available illumination;
the tests establish acquisition and recovery, not optical exposure accuracy.
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
.reference/inspection-venv/Scripts/python.exe scripts/inspection/validate_asi662.py --extended-only --output artifacts/inspection/asi662-new-600s.jsonl
```

`--capture-662` selects only the ASI662MC PID and requires exactly one matching
camera. Production connections use saved serials to distinguish multiple units.
The SDK remains available for its wider feature set.
