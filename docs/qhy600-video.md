# QHY600M Direct USB video

The pipe worker supports opt-in `mode: "video"` for the traced QHY600M U3G20
revision. It uses Rust and the Windows Cypress system driver, without loading
the QHY SDK. This is the worker protocol used by pier-camera clients; NINA,
ASCOM and Alpaca continue using their existing single-exposure path.

Initial scope: photographic mode 0, full 9600 × 6422 RAW8 or RAW16, bin 1, 1 µs–30 s
exposure. Video requires `dark: false`; cover the sensor physically for dark
video. Video ROI, hardware binning and other read modes are not exposed.
The broader [SDK video tests](qhy600-development.md#sdk-video-acceptance-2026-11-03)
are reference evidence, not additional Direct USB capabilities.

## Protocol

Launch `regain-device qhy camera-direct --serve`. Open the exact camera ID using
the existing framed JSON protocol. `open` advertises `captureModes`,
`videoBitDepths`, `videoMaxExposureMicroseconds`, `videoGeometry` and `videoFpsScope`.
Repeat `start` → `status` → `download` with parameters such as:

```json
{
  "mode": "video",
  "width": 9600, "height": 6422, "bin": 1, "x": 0, "y": 0,
  "microseconds": 100000, "dark": false, "gain": 30, "maxFps": 2,
  "bitDepth": 8
}
```

`bitDepth` defaults to 16; select 8 for native camera RAW8 with one byte per
pixel (61,651,200 bytes per full frame). RAW16 uses two little-endian bytes per
pixel (123,302,400 bytes). Reply metadata reports the actual `bitDepth` and
`bytes`; clients must honor these instead of assuming every image is RAW16.
Still captures remain RAW16 and reject `bitDepth: 8`.

Matching settings keep the stream running. `maxFps` defaults to 1 and accepts
0.01–120, including fractional rates. It limits **delivery**, not sensor output
or USB reads. The owner continues draining while delivery is paced or the client
pauses; stale completed frames are replaced in a bounded latest-frame buffer.

Continuous clients can use `stream-start` with the same parameters, followed by
`stream-poll`, `stream-status`, and `stream-stop`. The coordinator is shared with
the ZWO pier cameras. Polls return either status without pixels or one complete
image. Slow/blocked output runs separately from the hardware owner. Metadata
includes settings generations, replacement counts and host receipt timing.

Exposure, gain or bit-depth edits restart the QHY stream at a frame boundary. The driver
drains transition data for the old plus new exposure duration and discards at
least two complete frames. Changing only FPS does not restart the camera.
`stop`/`close` cancel and drain pending USB I/O before freeing its buffers.
A later still capture restores single-frame initialization. Stop video before
moving a camera-connected wheel; moves remain blocked between video requests.

## Validity and recovery

Each image requires a matching start/footer pair at exactly the full wire-frame
length. RAW8 has a shared frame delimiter: the footer of one frame is also the
header of the next. RAW16 has separate start/end markers. Partial following
frames and the shared RAW8 delimiter are retained across USB transfers. The
driver removes 34 extra wire rows and converts RAW16's big-endian pixels to
little-endian. RAW8 bytes need no endian conversion.

Video never replays a retained frame: `readRetries` must be absent or zero.
Bad framing, failed USB I/O or a frame deadline faults acquisition and prevents
cached images being delivered as success. Stop/restart is explicit;
`videoFrameRecoveryAttempts` is zero. Automatic USB reset is not implemented.

`video.rawSequence` is a host counter, not a hardware frame ID. Receipt estimates
are not sensor timestamps. Consumers needing a post-event frame must honor
settings generations and the reported freshness margin; the driver does not
infer that an external move or slew has completed.

## Physical acceptance

All 20 SDK-reference live frames in each format matched the decoded USB payload
byte-for-byte (40 images total).
The direct acceptance uses full-resolution images and unique hashes, repeated
captures, fractional pacing, paused/blocked consumers, settings changes,
pending-read cancellation, streaming and return to five-second stills. Images
are validated and discarded rather than saved.

The 2026-11-03 acceptance passed with 39 distinct full-resolution frames,
including three five-second stills after stopping video. During a four-second
blocked image write, acquisition advanced from 13 to 17 frames and replacements
from two to five. Both pending-read cancellation and cancellation during a
100-second delivery interval completed below the three-second limit. The active
direct worker loaded no QHY or ZWO SDK DLL.

The subsequent RAW8 extension passed the expanded acceptance in both starting
formats: 51 distinct full-frame images each (102 total), including RAW8 ↔ RAW16
switches through both request and continuous-stream APIs, blocked output,
cancellation and return to RAW16 stills. Evidence is in `direct-video-raw8-01`
and `direct-video-raw16-03`. Full-sensor framing and corruption regression tests
also pass.

```powershell
cargo build --release -p regain-device --locked -j 2
python scripts/inspection/qhy_direct_video_acceptance.py --output artifacts/qhy600/NEW-DIRECT-VIDEO-RUN
python scripts/inspection/qhy_direct_video_acceptance.py --bits 8 --output artifacts/qhy600/NEW-RAW8-VIDEO-RUN
```

Run only with an idle, operator-authorized camera. The script selects
`QHY600M-96763b4ab6a79e117`; adapt that ID explicitly for another admitted unit.
Local evidence is under `artifacts/qhy600/video-wire-m0-16-01` and
`artifacts/qhy600/direct-video-*`. This does not establish overnight stability,
physical cable-loss recovery, other revisions or optical calibration.

## Further modes

SDK reference tests succeeded for high gain (1), extended full well (2),
extended full well 2CMS (3), and 2CMS-0/1 (8/9), in both bit depths. Exposing
these through Direct USB still needs per-mode command traces, configuration
validation and switching tests. Video ROI would need equivalent tracing and
geometry/framing tests. Hardware 3×3 binning (5) timed out in both SDK formats;
it needs investigation before implementation. Fiber-only modes are outside the
attached USB camera's tested scope.
