# QHY600M Direct USB video

The pipe worker supports opt-in `mode: "video"` for the traced QHY600M U3G20
revision. It uses Rust and the Windows Cypress system driver, without loading
the QHY SDK. This is the worker protocol used by pier-camera clients; NINA,
ASCOM and Alpaca continue using their existing single-exposure path.

Initial scope: photographic mode 0, full 9600 × 6422 RAW16, bin 1, 1 µs–30 s
exposure. Video requires `dark: false`; cover the sensor physically for dark
video. RAW8, video ROI, hardware binning and other read modes are not exposed.
The broader [SDK video tests](qhy600-development.md#sdk-video-acceptance-2026-11-03)
are reference evidence, not additional Direct USB capabilities.

## Protocol

Launch `regain-device qhy camera-direct --serve`. Open the exact camera ID using
the existing framed JSON protocol. `open` advertises `captureModes`,
`videoMaxExposureMicroseconds`, `videoGeometry` and `videoFpsScope`.
Repeat `start` → `status` → `download` with parameters such as:

```json
{
  "mode": "video",
  "width": 9600, "height": 6422, "bin": 1, "x": 0, "y": 0,
  "microseconds": 100000, "dark": false, "gain": 30, "maxFps": 2
}
```

Matching settings keep the stream running. `maxFps` defaults to 1 and accepts
0.01–120, including fractional rates. It limits **delivery**, not sensor output
or USB reads. The owner continues draining while delivery is paced or the client
pauses; stale completed frames are replaced in a bounded latest-frame buffer.

Continuous clients can use `stream-start` with the same parameters, followed by
`stream-poll`, `stream-status`, and `stream-stop`. The coordinator is shared with
the ZWO pier cameras. Polls return either status without pixels or one complete
image. Slow/blocked output runs separately from the hardware owner. Metadata
includes settings generations, replacement counts and host receipt timing.

Exposure or gain edits restart the QHY stream at a frame boundary. The driver
drains transition data for the old plus new exposure duration and discards at
least two complete frames. Changing only FPS does not restart the camera.
`stop`/`close` cancel and drain pending USB I/O before freeing its buffers.
A later still capture restores single-frame initialization. Stop video before
moving a camera-connected wheel; moves remain blocked between video requests.

## Validity and recovery

Each image requires a matching start/footer pair at exactly the full wire-frame
length. Partial following frames are retained across USB transfers. The driver
removes 34 extra wire rows and converts big-endian pixels to common RAW16.

Video never replays a retained frame: `readRetries` must be absent or zero.
Bad framing, failed USB I/O or a frame deadline faults acquisition and prevents
cached images being delivered as success. Stop/restart is explicit;
`videoFrameRecoveryAttempts` is zero. Automatic USB reset is not implemented.

`video.rawSequence` is a host counter, not a hardware frame ID. Receipt estimates
are not sensor timestamps. Consumers needing a post-event frame must honor
settings generations and the reported freshness margin; the driver does not
infer that an external move or slew has completed.

## Physical acceptance

All 20 SDK-reference live frames matched the decoded USB payload byte-for-byte.
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

```powershell
cargo build --release -p regain-device --locked -j 2
python scripts/inspection/qhy_direct_video_acceptance.py --output artifacts/qhy600/NEW-DIRECT-VIDEO-RUN
```

Run only with an idle, operator-authorized camera. The script selects
`QHY600M-96763b4ab6a79e117`; adapt that ID explicitly for another admitted unit.
Local evidence is under `artifacts/qhy600/video-wire-m0-16-01` and
`artifacts/qhy600/direct-video-*`. This does not establish overnight stability,
physical cable-loss recovery, other revisions or optical calibration.
