# QHY600 implementation and acceptance

Direct Windows USB and an isolated QHY SDK backend share Regain's camera supervisor. Physical acceptance on 2026-11-03 covered both backends through the supervisor, NINA, the native ASCOM camera implementation and Alpaca HTTP. This is a development implementation, not a published release. Support for other QHY600 revisions and every SDK mode is not claimed.

## Reference hardware

| Property | Measured value |
|---|---|
| SDK model / ID | QHY600M / QHY600M-96763b4ab6a79e117 |
| Full hardware UID | 986a7d6303b843aabb6da4789ee51117 |
| Operational USB | VID 1618, PID C601 |
| Product / firmware | QHY600U3G20-20241018 / C2 date bytes 18 0a 12 |
| Sensor output | 9600 × 6422 monochrome RAW16, 3.76 µm |
| SDK effective area | x=24, y=34, width=9576, height=6388 |
| Current acquisition mode | PhotoGraphic DSO 16BIT (mode 0), bin 1 |

SDK: `C:\Program Files\QHYCCD\AllInOne\sdk\x64\qhyccd.dll`, SHA256 `5f0957e29ff510f19fa5de8688162fb4b9f4562b0b79b693dcbb9377bf281d14`.

The QHY Windows driver is required in both modes. PID C600 loads firmware and reenumerates as C601; its interface is `AE18AA60-7F6A-11D4-97DD-00010229B959`. Installing it and reconnecting with external power restored this camera without a Windows restart. SDK fallback uses the installed DLL or `REGAIN_QHY_SDK`. No QHY binaries are redistributed here.

Direct mode checks the firmware date and traced EEPROM model signature before writes. Unrecognized revisions require SDK mode until traced. The legacy SDK display ID loses hexadecimal digits; preserve the raw UID and reject ambiguous display IDs.

## Shared implementation

- `regain-transport::cypress`: driver enumeration and bounded overlapped transfers. Outstanding I/O reaches terminal completion before freeing buffers; a non-draining driver terminates its isolated worker.
- `regain-core::thermal`: the bounded PI cooler policy used by ZWO, with a live power-change limit, default 25 percentage points per five seconds.
- `regain-core::diagnostics`: circular memory/disk history, error-triggered batches and live off/circular/verbose modes.
- `regain-worker::camera`: bounded framed JSON replies and separate image bytes.
- `regain-qhy`: QHY identity, USB commands, SDK ABI and acquisition lifecycle.

NINA, native ASCOM camera slots and Alpaca camera slots route canonical QHY models to `regain-device qhy camera-direct` or `camera-sdk`. Existing ZWO profiles remain compatible. Select the complete QHY camera ID. QHY privileged USB-reset escalation is disabled; ZWO identity/reset logic must never be applied to it.

## Traced protocol

Passive instrumentation attaches only to our SDK probe, not NINA. `trace-qhy.js` hooks KernelBase once, avoiding duplicate Kernel32 forwarding events. The Python probe uses its base interpreter because a Windows venv redirector otherwise causes attachment to the wrong process. Command payloads and timings are recorded; full image contents are separate opt-in reference artifacts.

| Request | Observed purpose / encoding |
|---|---|
| C0 C2 | Firmware date in first three bytes; later bytes vary |
| C0 CA, value 0, index 8 | EEPROM model page; fields not fully decoded |
| C0 CA, value/index 16 | Full 16-byte hardware UID |
| 40 D1 / A0 | Single-frame initialization |
| 40 D1 / A2 | Full-sensor vertical layout including 34 extra wire rows |
| 40 D1 / A3 | Exposure microseconds, big-endian u32 |
| 40 D1 / A4 | Gain/read-mode fields; mode 0 boundaries traced |
| 40 D1 / A5 | SDK USB traffic parameter, 0–60 |
| 40 D1 / A6 | Exposure start |
| 40 D1 / A7 | RAW16 selection |
| 40 D1 / A8 | Offset multiplied by 16, big-endian u16 |
| 40 D1 / A9 ff | Observed initialization field; meaning not established |
| B9 register 62 | Initialization handshake, about 1.4 s inside transfer |
| B9 register 27, value 5 | Observed mode initialization handshake |
| B9 register 31, value 1 | Acquisition handshake, about 556 ms inside transfer |
| B8 register 19e, value 81 | Observed bulk-read setup |
| C0 BC, index 4 | DDR occupancy, little-endian u32, units of 32 bytes |
| B9 register 23, value 0 | Observed end-of-read cleanup |
| C0 F2 | Temperature sign, big-endian tenths, native PWM byte |
| 40 F4 | Manual cooling: big-endian u16 floor(native PWM / 2) |

Unexplained register roles are observations, not inferred specifications. Unused SDK stack garbage and dummy register data are zeroed. Host delays count from completion: hardware handshake durations must not be added again as sleeps. SDK initialization also reads one 16 KiB endpoint block.

Endpoint 81 uses 81,920-byte reads and a 512 KiB driver transfer setting. Start marker: `55 55 44 44 ee 11 dd 22`. Wire image: 9600 × 6456 × 2 bytes. End marker: `ee 11 dd 22 55 55 44 44`. Removing 34 leading rows and swapping 16-bit byte order reproduced **all bytes of three full SDK frames**, with no additional pixel correction.

The transport can emit a start marker in a short preamble, followed about 4–5 KiB later by the actual frame marker. Accepting the first marker caused false frame failures after SDK/direct switching and cancellation. The parser now requires a matching footer at exactly the full wire-frame length and limits candidate searches to the first 4 MiB. A full-sensor regression test covers the preamble, split footer, corrupted footer and rejection of a later exposure.

Downloads are consumptive: no same-exposure reread is advertised. Invalid boundaries fail closed; replacement exposures are explicitly reported. Successful captures do not reinitialize. Repeated captures, cancellation and SDK/direct switching passed without unexpected replacement exposures.

The SDK can report `GetQHYCCDExposureRemaining <= 100` immediately after starting a five-second exposure. Its download then blocks through integration. The SDK worker also checks elapsed integration time before downloading, keeping the owner available for thermal control and cancellation during exposure.

## Capabilities and evidence

Current worker: mode 0, bin 1, RAW16, software ROI from full-sensor captures, gain, offset, USB traffic, temperature, cooler power and target. Modes 1 and 2 have reference captures but are not frontend capabilities. Video, hardware binning, dew heater, Linux direct USB and retained-frame rereads are not claimed.

### SDK video acceptance (2026-11-03)

Tested the attached `QHY600M-96763b4ab6a79e117` with the installed SDK
(`qhyccd.dll` SHA256 `5f0957e29ff510f19fa5de8688162fb4b9f4562b0b79b693dcbb9377bf281d14`).
Each successful case captured two ten-frame live runs on the same open handle,
with `StopQHYCCDLive` between runs. Exposure was 100 ms, USB traffic was the SDK
default 50, and every frame used the entire 9600 × 6422 output. All 240 returned
frames had the expected format and distinct SHA256 hashes. No ROI was used.

| SDK read mode | RAW8 | RAW16 | Median frame interval, 8 / 16 bit |
|---|---|---|---|
| 0: PhotoGraphic DSO 16BIT | 20 passed | 20 passed | 478 / 759 ms |
| 1: High Gain Mode 16BIT | 20 passed | 20 passed | 474 / 764 ms |
| 2: Extend Fullwell Mode | 20 passed | 20 passed | 477 / 761 ms |
| 3: Extend Fullwell 2CMS | 20 passed | 20 passed | 733 / 1168 ms |
| 8: 2CMS-0 | 20 passed | 20 passed | 729 / 1166 ms |
| 9: 2CMS-1 | 20 passed | 20 passed | 732 / 1170 ms |
| 5: Bin3*3Mode (hardware), full 3200 × 2144 output | No frame | No frame | Timed out after 30.1 s |

Intervals exclude the first frame of each run and include SDK polling, tracing,
and hashing overhead; they are observed test cadence, not maximum frame rates.
The hardware-bin mode accepted setup/start but returned no successful frame in
either format. Both attempts stopped and closed successfully. This is a failed
test of that mode with this setup, not proof that all QHY600 variants lack it.
Modes 4, 6, 7 and 10 are labeled **Fiber Only** by this SDK and were not attempted
on the USB connection.

The probe uses `SetQHYCCDStreamMode(1)` before `InitQHYCCD`, then bits, bin/ROI,
exposure, `BeginQHYCCDLive`, bounded `GetQHYCCDLiveFrame` polling, and
`StopQHYCCDLive`. A fresh open selects a different read mode. The passive USB
trace shows `D1 A0 00` for live initialization versus `D1 A0 01` for stills;
`D1 A7 00/01` selects 8/16-bit output. The live path does not repeat the still
`A6`/`B9:31` start handshake for each frame. Do not reuse the still lifecycle for
video. Failed live polls return `0xffffffff` even during healthy acquisition;
future production support needs a bounded frame deadline, not immediate camera
reinitialization on every such result.

Evidence: `artifacts/qhy600/video-m<mode>-<bits>-01/{sdk,usb}.jsonl` and
`video-summary.json`. SDK calls and USB commands/timings are recorded; live image
payloads are hashed in memory rather than written into the trace. Reproduce one
case with the existing Frida-enabled probe environment:

```powershell
artifacts/qhy600/venv/Scripts/python.exe scripts/inspection/qhy_sdk_probe.py --live --mode 0 --bits 16 --frames 10 --seconds 0.1 --output artifacts/qhy600/NEW-VIDEO-RUN
```

These are **SDK reference tests**, not Regain frontend video acceptance. QHY
video and these additional read modes remain unexposed in NINA, ASCOM, Alpaca,
and the direct USB worker.

### Review: image memory pressure

Review found infallible full-image allocation in both backends, plus redundant
full-frame copies during direct unpacking and worker ROI packing. The SDK asks
for 253,053,600 bytes even though a delivered RAW16 frame is 123,302,400 bytes.
Its allocation failure was observed during the earlier installed-NINA test
while a full Rust build was competing for memory; the worker aborted instead of
returning a diagnostic error.

QHY frame buffers now use fallible reservation, direct capture reserves its
bounded frame/preamble window before draining USB, and unpacking/cropping reuse
the original allocation. Full-frame delivery avoids another image allocation.
Tests verify every pixel of a full-sensor overlapping crop, allocation reuse,
invalid geometry, and a rejected allocation request. This reduces peak memory
and makes these allocation failures reportable; it does not promise recovery
from arbitrary out-of-memory failures inside the vendor SDK or frontend.

After the review fix, all 1,135 Rust workspace tests passed with two build jobs;
targeted clippy passed with warnings denied. The rebuilt production supervisor
then captured four full 9600 × 6422 RAW16 five-second frames per backend,
including a fresh frame after cancel/restart, with zero replacement exposures,
download retries or fallback. This also verified returning from SDK video to
ordinary still capture through a fresh connection. Evidence is in
`review-workspace-tests.log`, `review-clippy.log`, `review-build.log`, and
`review-stills-{direct,sdk}-01` under `artifacts/qhy600/`. Builds and physical
capture were run sequentially.

Ignored evidence under `artifacts/qhy600/`:

- `sdk-reference-wire`: three full five-second frames and exact wire comparison.
- `sdk-mode0-controls-thermal`: gain/offset/traffic boundaries and manual PWM.
- `sdk-auto-cooler`: automatic cooling and final power-off.
- `sdk-mode1-controls`, `sdk-mode2-abort`: additional modes and cancellation reference.
- `direct-reference-03`: three full direct frames, approximately 6.5 s each.
- `supervisor-direct-*`, `supervisor-sdk-*`: production IPC, full frames, hashes, cancellation and recovery counts. Unexpected replacement exposures fail acceptance even if a later image succeeds.
- `supervisor-direct-final`, `supervisor-sdk-final`: five full frames per backend including cancel/restart and a reopen seeded with the prior 53% cooler demand. Both restored cooling before the final exposure, with zero replacement exposures. After SDK disconnect the independent F2 power readback declined gradually and reached zero (`final-idle-inspection.jsonl`); zero-power acknowledgement is not an assertion of instantaneous zero output.
- `nina-physical-01.log`: two opt-in physical tests, one per backend; four full frames each, cancellation/restart, negative temperatures, live logging/ramp changes and stable worker identity. Direct workers had no QHY SDK DLL loaded.
- `installed-nina`: three FITS files per backend from installed NINA, plus a final SDK capture. All were 9600 × 6422 RAW16 at five seconds. The six-image comparison had distinct pixel hashes, `INSTRUME=QHY600M` and the selected QHY ID in `CAMSER`. NINA's maximum-exposure display was fixed to use 64-bit microseconds and verified at 3600 s.
- `ascom-physical`: three full frames per backend through the real `ICameraV4` implementation, about 7.3–7.4 s each. This harness instantiates the production class without changing COM registration; a newly installed Chooser entry was not retested.
- `alpaca-direct-02`, `alpaca-sdk-01`: three full frames per backend over HTTP ImageBytes, approximately 7.2–7.4 s each, valid headers and zero replacements.
- `nina-tests-01.log`: 689 passed, five opt-in tests skipped. `unit-tests-02.log` covers core/transport/worker regression tests; `qhy-tests-final.log` includes the full-sensor framing regression. Clippy passed with warnings denied.

Temporary installed-NINA binaries, global Regain settings, the isolated profile and acceptance fixture were restored or archived after testing. No vendor SDK was redistributed and no release was published.

```powershell
cargo build -p regain-device -p regain-alpaca --release
python scripts/inspection/qhy_acceptance.py --backend direct --cooling-recovery --output artifacts/qhy600/NEW-DIRECT-RUN
python scripts/inspection/qhy_acceptance.py --backend sdk --cooling-recovery --output artifacts/qhy600/NEW-SDK-RUN
```

## Acceptance scope

- [x] Identity, SDK flow and complete wire layout.
- [x] Repeated full-frame direct and SDK captures through the supervisor.
- [x] Shared transport, diagnostics, thermal policy and frontend routing compile.
- [x] No unexpected replacement after cancellation or SDK/direct switching.
- [x] Cooler convergence below zero, live rate changes, signed temperature and disconnect-off.
- [x] Full-frame NINA capture, FITS identity and cancel/restart acceptance.
- [x] Alpaca HTTP ImageBytes and the native ASCOM camera implementation.
- [x] Explicit first-release scope: traced U3G20 firmware, photographic mode 0, RAW16, bin 1.
- [x] Crate package contents and user documentation; no SDK redistribution.

Remaining release work is the normal review, signed-package build and installer acceptance. Broader revisions, other read modes, video, hardware binning, Linux direct USB and privileged USB-reset integration are separate extensions. Physical tests used five-second full frames; the advertised one-hour SDK limit was not tested with hour-long integrations.
