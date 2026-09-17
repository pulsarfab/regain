# ASI6200MM Pro (non-P25)

The owner identified the camera tested on 2026-09-17 as a non-P25 model.
It was capped for dark captures. The SDK reports `ZWO ASI6200MM Pro`:
9576 × 6388 monochrome pixels, 3.76 µm, 16-bit, bins 1–4, and exposures
from 32 µs to 2,000 seconds. SDK mode remains the default.

See the [sanitized test evidence](asi6200-original-evidence.json) and
[NINA image-pane tests](nina-end-to-end.md#asi6200mm-pro-non-p25--2026-09-17).

## Revision detection

Both tested ASI6200 editions use USB ID `03c3:620b`, USB3 endpoint `81`,
1024-byte packets and burst 15. The PID does **not** identify the edition.
Read vendor register `BC:1c` before configuring the sensor:

| Observed register | Edition | HMAX at bandwidth 40 | Fan/LED controls |
| --- | --- | --- | --- |
| 3 | Non-P25 | 1515 | Not advertised by SDK; omitted in direct mode |
| 5 | P25 | 880 | Available |

The driver rejects other revision values; optional SDK fallback can handle
them. Detection reads the camera each time it is opened. No SDK, serial-number
list or user-selected edition is needed by the direct backend.

The SDK 1.41 x64 branch at RVA `1ee699` tests this register for value 5 and
selects HMAX 880. The original table value is 1515. These observations are
version-specific research; production code reads USB registers and never calls
private SDK functions. The SDK DLL SHA-256 is
`0c8778c3cce2012961b079e3c7d0d8348a8b3823939335d9e98148cb5d5dc34a`.

## Capture behavior

The normal RAW16 format table matches the P25 trace exactly. Initialization
differences are the line timing and saved gain, offset and exposure values.
The direct path shares the sensor setup, sets the revision's HMAX, then applies
the requested controls. Both use a 20 MHz timing clock, 52 blanking rows,
FPGA divisor 400 at bandwidth 40, and host timing for exposures of at least
one second. Unit-test fixtures come from independent SDK captures.

The readout wait also uses the detected line period: 75.75 µs for the original
camera and 44 µs for P25. A ready flag alone does not prove every sensor row
has reached DDR. Small physical ROIs retain the 128 KiB minimum, followed by
factory correction, cropping and software binning.

The same-frame processing comparison passed for a full 122,342,976-byte frame,
including the independently decoded factory correction map. All 42 eligible
Camera Kit samples also matched the SDK byte-for-byte, covering ROI, bins,
timing and control changes. Separate dark exposures cannot establish pixel
equality; these comparisons use the exact USB frame returned by the SDK.

The SDK extended workup passed 46/46 exercises and restored every saved control.
Its SDK-only flip, hardware-bin and high-speed exercises are not native features.
The production native path remains normal RAW16 with software binning.

## Recovery validation

The native matrix passed 66 cases (68 frames): full-frame repeats, bins 1–4,
small/moved/edge ROIs, gain boundaries, offset limits and 32 µs–60 s exposures.
Full-frame control transitions passed vertical-band checks for stale rows.
Interrupted reads and a real USB timeout recovered identical retained pixels.

Cancelling the first or last bulk request with Windows `CancelIoEx` produced
terminal error 995, then a valid frame without another exposure. The last-chunk
test matched 121,633,888 previously downloaded interior bytes. Trace hashes
exclude four bytes at each end of every USB chunk; the worker's complete-frame
comparison also matched all pixels outside the changing frame envelope. A separate 5 ms
transfer-budget test returned a structured error with zero image bytes, then
accepted a fresh, explicitly requested full-frame exposure.

Worker termination with cooling enabled passed through the shared Rust
supervisor in all three backend configurations:

| Backend after recovery | Prior temperature | Restored setpoint | Temperature after recovered image |
| --- | --- | --- | --- |
| SDK | 26.5°C | 20°C | 25.3°C |
| Direct | 24.8°C | 16°C | 23.4°C |
| Direct → SDK fallback | 23.1°C | 14°C | 19.0°C |

Every run restored the target and dew setting, waited for prior cooling
performance and completed the replacement exposure before reaching the target.
Cooling and dew were off afterward. The non-P25 camera has no SDK fan/LED
controls, so those were neither advertised nor written in the native tests.

Retries remain bounded. Two consecutive cancellations during a 60-second frame
were followed by a timeout and used up the default two read retries. The
supervisor returned an error without SDK fallback or another exposure, as
required by the default 30-second recapture limit. A retained frame is not a
guarantee that every sequence of USB errors can be recovered within that budget.

A separate 60-second exposure recovered from one cancelled transfer through
NINA's shared supervisor. It used both read retries (the first restart timed
out), delivered the full frame and logged recovery. There was one exposure
start, one worker and no SDK fallback. With only one read retry, a 31-second
exposure with two cancellations failed with no image or replacement exposure.

## Reproduce

Disconnect the camera in NINA and other capture applications first. Use a new
output path for each run. Raw traces and calibration stay local under `artifacts/`.

```powershell
.reference/inspection-venv/Scripts/python.exe scripts/camera-kit/camera_kit.py --camera 'ZWO ASI6200MM Pro' --profile extended --include-pixels --exercise-cooling --deadline 1200 --output artifacts/NEW-6200-sdk
.reference/inspection-venv/Scripts/python.exe scripts/inspection/validate_6200_kit.py artifacts/NEW-6200-sdk/RUN-DIRECTORY --worker target/release/zwogain-direct.exe --output artifacts/NEW-6200-parity.json
.reference/inspection-venv/Scripts/python.exe scripts/inspection/validate_asi6200.py --worker target/release/zwogain-direct.exe --full-controls --output artifacts/NEW-6200-direct.jsonl
.reference/inspection-venv/Scripts/python.exe scripts/inspection/validate_environment.py --camera-name 'ZWO ASI6200MM Pro' --output artifacts/NEW-6200-cooling
```

These dark-frame tests do not establish illuminated geometry, photometric
performance, physical unplug retention or cold-power startup. Automatic USB
handle reopening remains restricted to the tested ASI2600 P25 path; the 6200
uses retained sender/pipe retries and bounded reconnect/recapture.
