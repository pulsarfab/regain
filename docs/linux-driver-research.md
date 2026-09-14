# Linux ZWO transport research

Reviewed 2026-09-14. I found useful open-source wrappers and partial reverse
engineering, but no verified, maintained SDK-free implementation for the
ASI2600/6200 P25 or ASI676 families among these projects.

| Project | What it provides | Use for ZWOgain |
| --- | --- | --- |
| [INDI ASI](https://github.com/indilib/indi-3rdparty/tree/master/indi-asi) | Its [build file](https://github.com/indilib/indi-3rdparty/blob/master/indi-asi/CMakeLists.txt) requires and links the ASI library, alongside USB and INDI libraries. | Application lifecycle and hotplug handling; not a replacement USB protocol implementation. |
| [Open Astro Project](https://github.com/openastroproject/openastro/tree/master/liboacam/zwo) | The [ZWO loader](https://github.com/openastroproject/openastro/blob/master/liboacam/zwo/ZWASI2dynloader.c) loads `libASICamera2` and resolves ASI API functions. | SDK integration reference; no independent frame-setup protocol. |
| [sidneycadot/ZWO-ASI-ReverseEngineering](https://github.com/sidneycadot/ZWO-ASI-ReverseEngineering) | ASI120MM-S research based on 2015 SDK 0.1.0803, libusb interception, sensor register notes and a partial replacement library. The replacement's ROI and exposure entry points are stubs. | Useful tracing methodology and older sensor facts; not runnable modern camera support. |
| [seeing-things/zwo](https://github.com/seeing-things/zwo) | SDK wrapper, partial FX3/buffer reverse engineering and a `zwo_fixer` shim. The inspected FX3 acquisition methods include unfinished stubs. | Concrete cancellation failure evidence and leads for inspecting SDK transfer ownership. |

## A concrete cancellation bug

The [zwo_fixer implementation](https://github.com/seeing-things/zwo/blob/master/zwo_fixer/zwo_fixer.cpp)
handles an older Linux SDK path where cancellation returns
`LIBUSB_ERROR_NOT_FOUND`, yet SDK logic waits 500 ms for a callback. The shim
updates internal completion state so that this path terminates. It is a
version-dependent SDK patch, not a replacement USB driver, and does not prove
the same bug exists in our Windows SDK 1.41.

The [libusb asynchronous I/O documentation](https://libusb.sourceforge.io/api-1.0/group__libusb__asyncio.html)
distinguishes requesting cancellation from completed cancellation. A submitted
transfer must retain its buffers until terminal completion. This supports our
existing drain-before-free rule and process deadline. A future Linux transport
should handle cancellation's return code explicitly and keep callback state per
transfer. USB packet-aligned buffers also matter when diagnosing overflow.

## Next experiments

1. Capture Linux libusb request/completion traces for the same exposure settings
   used on Windows. Compare vendor requests, lengths, timeouts and frame markers.
2. Inspect the Linux library's FX3 setup, submission and completion paths where
   retained symbols make call relationships easier to identify.
3. Validate retained-frame restart on each model, distinguishing a partial read,
   terminal transfer failure and physical reconnect. Do not assume retention
   survives reconnect or that a second SDK download call re-reads the device.
4. Keep model initialization, calibration and cooler control separate from the
   common transport. The Duo hardware observations below demonstrate why.

No third-party code was copied into the Apache-licensed Rust implementation.

## Attached ASI2600MM Duo: first hardware observations

The SDK enumerates a 6248 × 4176 ASI2600MM Duo main sensor and a separate
1920 × 1080 ASI220MM Mini guide sensor. Direct descriptor probes, without the
SDK loaded, find the same Cypress-style Windows interface and driver version
`0x01020200`, but different USB configurations:

| Sensor | VID:PID | Bus | Bulk IN | Packet / burst |
| --- | --- | --- | --- | --- |
| ASI2600MM Duo | `03c3:2601` | USB 3.0 | `81` | 1024 bytes / 15 |
| ASI220MM Mini | `03c3:2209` | USB 2.0 | `81` | 512 bytes / no SuperSpeed companion |

Both completed capped RAW16 SDK dark-frame tests at 512 × 256 and full sensor
resolution. The main sensor uses the `5a7e` / `3cf0` frame-envelope family;
the guide sensor's observed first/last dwords are `11aa00bb` / `bb00aa11`
in byte order. The common ASI676 correction hooks did not execute for either
model, so those processing paths still need separate inspection. The main
sensor did read calibration through `C3` at `0x40000`.

These results establish shared Windows transport access, not SDK-free Duo
acquisition or P25 compatibility. The ASI676 capture entry point remains
restricted to PID `676d`. Duo sensor setup, correction, cooling and retention
will be validated with model-specific traces before enabling direct capture.
