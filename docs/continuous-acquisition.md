# Continuous acquisition (development)

The ASI SDK and Direct USB pipe workers expose an **opt-in** continuous path.
Existing `start/status/download` single-exposure clients are unchanged. Merely
updating Regain does not switch AutoPierCam or NINA to this new path.

The capture owner drains frames independently of IPC reads, output-pipe writes
and the delivery FPS limit. A single latest-frame slot replaces older frames;
there is no growing image queue or catch-up burst. The I/O thread never calls
camera discovery, control or capture APIs. SDK calls remain on one owner thread;
Direct USB retains its exclusive hardware worker.

## Protocol

After `open`, inspect `continuousAcquisition` and the existing `captureModes`.
Configure gain, white balance and other controls **before** starting the stream.

`stream-start` takes the existing exposure/ROI fields, plus `maxFps` (0.01–120,
default 1). With `mode` omitted, it chooses supported native video within its
exposure limit, otherwise repeated still captures. Explicit `mode: "video"`
does not silently fall back if unsupported. Dark/shutter exposures select still
mode automatically. The result and `stream-status` report the actual mode.

- All SDK cameras use `ASIStartVideoCapture` / `ASIGetVideoData` for video.
  An unchanged stream does not rewrite ROI, exposure or white balance per frame.
- Direct ASI662MC/ASI676MC reuse their established native video protocols,
  currently limited to 30 seconds. Their worker skips acquisition FPS sleeps
  in continuous mode, including during the existing bounded framing recovery.
- Other Direct families, and exposures beyond that video limit, use repeated
  still captures. **This is not a claim of native video protocol support** for
  ASI220, ASI2600 or ASI6200, or hardware validation of this new continuous path.

Poll `stream-status` (cached state, no discovery or extra handle). `ready` means
a latest frame is available and the delivery interval has elapsed. Call
`stream-download` only when ready. It consumes that frame, never duplicates it.
The metadata includes acquisition sequence and settings generation. Status
reports acquired, delivered and replaced frame counts, frame age, current mode,
requested exposure and any terminal error. Replaced frames are deliberate
consumer decimation, **not measured USB/SDK dropped frames**.

Repeating `stream-start` with only `maxFps` changed adjusts delivery pacing,
without reconfiguring capture. A new ROI/exposure request stops the video session,
discards the host's buffered frame and starts a new settings generation. Raw
legacy commands (including discovery and controls) are rejected during the stream.
Stop before changing gain or WB. This boundary policy is intentionally conservative;
it is not yet equivalent to ASICap's live scalar-control updates.

`stream-stop` immediately stops native video when the backend returns. In
repeated-still mode it reports `stopping: true`: the owner drains the pending
exposure and stops **without starting another**. Wait for `active: false` before
changing settings or closing. A still-mode settings change also requires that
boundary stop first. Forced interruption of a non-cancellable still capture
continues to require terminating the isolated host. Cleanup failure latches a
fault and requires host replacement; it must not start more captures.

SDK video polling uses the documented `2 × exposure + 500 ms` wait, capped at
1 second per call so the same owner can service requests. A normal timeout is
not an exposure failure; retries back off 100 ms, without stopping the stream.
Delivered SDK video metadata includes the cumulative timeout count. An independent no-frame deadline is `2 × exposure +
30 seconds`; other SDK errors remain terminal. Clients must allow more than
one second for ordinary stream commands and retain process-level watchdogs for
SDK/kernel calls that fail to honor their deadlines. No USB reset is implicit.

Diagnostics record configuration/stop duration, first-frame latency for each
generation, a progress summary at most every 30 seconds of frame delivery to
the owner, and terminal failure. These contain no image pixels or camera identity.

## ASICap comparison and exposure timing

Passive public-SDK-export observation on an ASI662MC/USB 2 found:

- Stable acquisition continuously calls `ASIGetVideoData`, without per-frame
  ROI/control writes or repeated discovery.
- Switching automatic exposure/gain to manual and changing exposure to 234 ms
  did not stop video. A RAW8-to-RAW16 change did stop/reconfigure/restart video.
- For that format change, ASICap briefly requested 100 ms before stopping,
  changed ROI/format, then restored 234 ms before starting video again.
- Video read wait arguments were 1437 ms around 468.53 ms exposure, then 968 ms
  around 234 ms. After the format restart the observed argument was 520 ms even
  though the last exposure readback/write remained 234 ms. Do not infer exposure
  duration from that wait argument alone.
- A matched RAW16/manual gain 300/234 ms segment returned 641 frames over about
  150 seconds without errors or reported drops. This short observation does not
  establish long-run reliability, timing at 25–60 seconds, or the remote failure's
  root cause. ASICap's SDK binary also differs from the bundled SDK despite the
  same file-version string.

Before implementing live exposure/gain updates, trace transitions in both
directions while recording control-call begin/end, video reads and first-frame
latency. Public SDK success alone does not establish which buffered frame first
uses new settings. Test that transition explicitly rather than associating every
frame after a write with the requested new exposure. The current generation
number fences **host buffers**, not proof of sensor exposure timing.

A subsequent live RAW16 trace changed 234 → 900 → 100 → 234 ms without
stopping video. On the 234 → 900 ms step, an in-progress read completed 108 ms
after the control write returned, having taken 233 ms; the following read took
900 ms. On the 900 → 100 ms step, an in-progress read completed 57 ms after the
write returned, having taken 897 ms; following reads took roughly 118–121 ms
(integration plus transport/cadence). This establishes overlap between control
writes and reads, **not optical proof of which sensor settings formed a frame**.
ASICap's displayed dropped-frame count reached one during this trace. Three
consecutive `ASIGetVideoData` calls returned timeout 11 after roughly 700 ms
each, with approximately 100 ms between retries. ASICap did not issue stop/start
calls and resumed successful reads. Settings were restored to RAW16, 234 ms,
gain 300 afterward. The trace was detached and no camera frames were saved.

## Verification

`cargo test -p regain-zwo --lib` covers latest-only storage, fractional delivery
FPS, settings generations, invalid requests, boundary stops and latched faults.
`python scripts/test-rust.py` exercises every Direct simulator and SDK simulation
over real pipes. Its inert SDK ABI fixture checks persistent video, transient
timeouts, terminal removal, no per-frame reconfiguration, and continued draining
while a large output frame blocks the pipe. No automated test opens a camera.

Real hardware soak/transition checks and AutoPierCam adoption remain required
before making this the production capture path.
