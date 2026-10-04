# ASI676MC Direct USB video

The isolated Direct USB worker supports opt-in `mode: "video"` for ASI676MC,
using the same acquisition engine as ASI662MC with a separate sensor profile.
Still capture remains the default. NINA, ASCOM and Alpaca do not automatically
select video; clients must opt in using the advertised `captureModes` capability.

Video supports RGGB RAW16, bin 1, up to 3552 × 3552, and 32 µs–30 s exposure.
The ASI676MC's own geometry, gain, offset and even-origin ROI restrictions remain
in force. Unsupported settings fail validation before acquisition.

See [the shared video semantics](asi662mc.md#experimental-continuous-video-source-builds)
for the `start`/`status`/`download` protocol, cancellable waits and `maxFps`.
FPS caps actual grabs (0.01–120), not sensor output; buffered frames can be older
than the current scene after a slow consumer resumes. Matching settings reuse
initialization and calibration. Exposure/gain/ROI changes restart the stream;
changing only FPS does not. Live video never uses retained-frame replay.

## Hardware evidence

Operator-authorized Windows SDK 1.41 reference traces covered three full-frame
100 ms captures and three 6.4 s captures on USB 2. The SDK uses bandwidth-40 word
`0x161c` and the same long-exposure trigger, low-power and XHS gating sequence
as the ASI662MC, with model-specific sensor initialization and timing.

The Direct worker passed 21 video frames on USB 2: full frames (25,233,408 bytes),
512 × 256 ROI, 0.5 FPS, slow consumers, exposure changes through 30 seconds,
cancellation, restart and return to still capture. Long-exposure cancellation
took about 219 ms; cancellation of a 100-second FPS wait was immediate at the
measurement resolution. Pixels were validated and discarded, not saved/uploaded.

USB 3 video validation is pending. These checks do not establish optical
accuracy, cold startup, physical-disconnect recovery, long-running stability or
Linux/macOS video hardware behavior. No private camera identity, image,
calibration payload or raw trace is committed or bundled.

## Explicit manual reproduction

Run only on an idle, operator-authorized camera. Default automated tests use
simulated devices and never run these hardware commands.

```powershell
cargo build -p regain-device --locked
target/debug/regain-device.exe zwo camera-direct --capture --video --max-fps 0.5 --frames 3
.reference/inspection-venv/Scripts/python.exe scripts/inspection/check_video.py --camera-name "ZWO ASI676MC" --output artifacts/asi676-video-new.jsonl
```

Selection requires exactly one matching model; multiple matching units are
rejected. The production pipe protocol also supports explicit serial selection.
