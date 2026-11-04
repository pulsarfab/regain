# QHYCFW3 XL bring-up

Hardware tested on 2026-11-03 through the **QHY600 camera's 4-pin connector**.
The user confirmed this connection after the initial standalone USB probe was
silent. COM3 was not established as a usable control path for this wheel.

The SDK reports nine slots; traced firmware is **20181114**. All nine slots
passed the initial 24-command SDK sweep, and full camera frames succeeded
between filter changes. The later production/NINA acceptance is recorded
below. Reset, calibration, firmware updates and USB cable removal were not
part of these passes.

## Camera-connected acceptance

Run `scripts/inspection/qhy_cfw_camera_probe.py` with the Frida environment from
camera bring-up. It attaches passive tracing only to its own SDK process.
Supply the exact parent camera ID and a new output directory. Modes:

- `--sweep`: ascending/descending positions, wraparound, long jumps, same-slot
  requests and return to the initial slot. All 24 commands passed, with settled
  readback checked twice. Completion plus verification took 1.0–5.875 s.
- `--captures`: settle at slots 1, 5 and 9, then take one full 9600 × 6422 RAW16
  five-second image at each. All three succeeded in 6.83–6.89 s, produced distinct
  pixel hashes and left the wheel at the expected position after download.
- `--reconnect`: move to slot 5, close/reopen and initialize the parent camera,
  verify the position and slot count, then return to the initial slot.

Evidence is under ignored `artifacts/qhycfw3/camera-{identify,sweep,captures,reconnect}-01`.
The probe uses the same installed SDK and hash recorded in [QHY600 development](qhy600-development.md).
No move is retried after ambiguous completion; a failure stops the test rather
than replaying a restoration move. The successful passes restore slot 1.

## Traced camera USB bridge

The camera and wheel share the same physical camera handle; there is no second
USB device to open for this connection.

| Transfer | Observed meaning |
|---|---|
| OUT 40/C1, value/index 0 | Forward ASCII command to the wheel |
| IN C0/C3, value/index 0 | Read the wheel response |
| `VRS`, 8-byte response | Firmware `20181114`; used by IsQHYCCDCFWPlugged |
| `MXP`, 1-byte response | `9`; GetQHYCCDParam(CONTROL_CFWSLOTSNUM=44) returns 9 |
| `NOW`, 1-byte response | Zero-based ASCII position `0` through `8` |
| `0` through `8` | Absolute move; SendOrder2QHYCCDCFW writes one byte |

SDK queries wait about 100 ms between C1 completion and C3 submission. Firmware
read took about 400 ms inside the transfer; settled position reads about 150 ms.
The SDK returns from move submission in roughly 16–27 ms, before motion ends.
During motion, it returns status `N`, not the last settled position. The trace
shows C3 completing with Windows error 31 and only the 38-byte driver header in
these cases; the SDK synthesizes `N`. The direct implementation must not
turn every I/O failure into “moving”: bounded pending state, identity checks
and disconnection errors must remain distinct.

The observed `MXP=9` is a count, despite the published table describing a max
index. Do not add one unconditionally. This pass verified positions 0–8 only.

## Observed connection

- Windows COM3, Silicon Labs CP2102 USB-to-UART bridge.
- VID 10c4 / PID ea60, USB serial `0001` (generic; not a unique wheel identity).
- Settings tried: 9600 baud, 8N1, no flow control, DTR/RTS disabled.
- `VRS` sent as three bare ASCII bytes; no reply within five seconds.
- A separate 30-second passive open received no startup bytes.
- Evidence: ignored `artifacts/qhycfw3/identify-01/serial.jsonl` and
  `artifacts/qhycfw3/passive-open.json`.

The [manufacturer's guide](https://www.qhyccd.com/qhycfw3-filter-wheel/)
distinguishes USB mode (red flash at startup) from camera/4-pin mode (green).
It also prohibits simultaneous USB and camera control connections. The user
confirmed camera/4-pin control; no further COM-port probes were performed.

## Acceptance procedure

`scripts/inspection/qhy_cfw3_probe.ps1` records every serial command and reply
with timings. It first queries firmware (`VRS`), carousel information (`MXP`)
and current position (`NOW`). Motion is an explicit `-Sweep` option and requires
the known physical carousel count through `-ExpectedSlots`.

The slot-count guard matters: the published protocol describes MXP as the
maximum zero-based index, while the upstream
[INDI implementation](https://github.com/indilib/indi/blob/master/drivers/filter_wheel/qhycfw3.cpp)
also interprets ordinary responses as counts. Preserve the raw response and
verify the actual carousel before issuing moves. The camera-connected SDK has
now identified this XL as the nine-position layout.

After identity is established, test every slot in ascending and descending
order, wraparound, long jumps, same-slot acknowledgements and a final return
to the initial slot. Require both a matching completion byte and a separate
idle NOW readback. On any ambiguous response, stop without replaying a move.
Then close/reopen and verify firmware, geometry and position again.

## Integration

The implementation is in `regain-qhy`. For this 4-pin connection, wheel
operations run through the existing camera owner in both direct and SDK modes.
There is no separate wheel worker competing for the camera's exclusive USB
handle. Frontend wheel connect/disconnect leaves an active camera owner
alive; camera recovery must verify the physical wheel position before resuming
exposure, without replaying an uncertain move. Retain an unknown/moving state
until a fresh stable readback confirms the expected optical state.

Standalone USB is implemented in `regain-qhy::cfw_usb`, using the common
`regain-transport::serial` layer. It does not need `qhyccd.dll`. Select an explicit
port and verify the protocol; a generic USB bridge is not a wheel identity.
The physical USB acceptance status is recorded below.

The locally installed `QHYCFW.dll` and `QHYCFW2st.dll` use camera-side shared
memory. The AllInOne directory also contains the standalone RS232 ASCOM
installer; it has not been installed or executed during this test.

## Regain production acceptance (2026-11-03)

`scripts/inspection/qhy_shared_wheel_acceptance.py` exercises the production
camera supervisor, rather than calling the vendor SDK probe directly. Direct
USB and SDK each passed a complete ascending/descending sweep, wraparound,
long and same-position moves, four full 9600 x 6422 RAW16 five-second captures,
and rejection of a move while exposing. A deliberately incorrect persisted
filter expectation blocked acquisition without moving the wheel; an explicit
move restored operation. No replacement exposures were needed for successful
frames. Artifacts: `regain-direct-03` and `regain-sdk-01`.

The first production pass exposed contention with background camera telemetry;
wheel requests now serialize with idle telemetry while capture requests still
reject moves. Physical testing also caught a transient matching reply before
settling. Both backends now require matching readbacks at least 250 ms apart,
with an unchanged 45-second deadline. Same-position requests perform readback
without issuing a motor command.

The installed NINA 3.2 pass used its real camera, wheel and imaging mediators:
all nine filters, six full FITS frames per backend, capture-time move rejection,
wheel-first/direct and camera-first/SDK connection, camera disconnect with
wheel-only movement, camera reconnection, and capture with the wheel frontend
disconnected. All twelve files had 9600 x 6422 RAW16 data, distinct pixel hashes,
`INSTRUME=QHY600M`, and the correct `CAMSER`. Connected-wheel captures had the
expected `FILTER`; NINA omits it while its wheel frontend is disconnected.
One physical camera worker served both devices; direct had no `qhyccd.dll`
loaded, SDK did. Evidence: `installed-nina`, including the NINA log and FITS
verification. Original installed plugin binaries and Regain settings were
restored and hash-verified; the fixture and temporary profile were archived.
The fixture disabled the separate NINA peripheral-recovery coordinator; the
shared camera worker's filter checks remained active. Physical cable removal
and reconnection were not exercised in this pass.

The native ASCOM ICameraV4/IFilterWheelV2 classes passed both backends: nine
positions, three five-second full frames at positions 0/4/8, move rejection
during capture, independent connection leases, and a fourth full frame after
reconnection. Cooling was enabled during imaging and acknowledged off when
the last camera client disconnected while the wheel remained open. QHY power
telemetry may ramp down after the zero-demand acknowledgement; the assertion
checks enable state rather than fabricating an instantaneous zero measurement.
Evidence: `ascom-direct-02` and `ascom-sdk`. These exercised the native classes;
they did not modify machine-wide COM registration or run the installer.

The HTTP Alpaca pass also passed Direct USB and SDK using
`scripts/inspection/qhy_alpaca_acceptance.py --wheel`. Discovery exposed the
camera-connected wheel, configured names and offsets matched, all nine slots
settled, and each backend delivered four full five-second ImageBytes frames.
Moves during acquisition were rejected; disconnecting either frontend left the
other usable. Successful frames required zero replacement exposures. Evidence:
`alpaca-direct-01` and `alpaca-sdk-01`. Both passes finished at position 0 and
closed their connection leases. Concurrent wheel-command admission is also
covered by the Alpaca unit test: an accepted optical command blocks exposure
admission until it finishes, including its error path.

## Standalone USB bring-up (2026-11-03)

After the user removed the camera control cable, COM3 remained present as
CP210x VID 10c4/PID ea60, USB serial 0001. The read-only VRS probe and the
production Rust worker both opened COM3 but received zero reply bytes at
9600 8N1, no flow control, DTR/RTS disabled. Evidence:
`artifacts/qhycfw3/usb-sweep-01/serial.jsonl`. The sweep stopped before any move.
After the user changed the recessed mode switch, the same port immediately
answered VRS=20181114, MXP=9 and NOW=0. The hardware passes below validate USB
separately from the earlier 4-pin tests.

The standalone protocol differs from the camera bridge: a move's serial
completion byte must be consumed before NOW is sent. The worker acknowledges
submission to the frontend immediately, reports position -1, and polls for that
completion without sending further serial commands. Completion must match the
target; two subsequent matching readbacks at least 250 ms apart establish idle.
The 45-second deadline is never extended by partial replies. Failed commands
are not replayed. Protocol tests cover pending completion, stable readback,
same-position suppression, wrong acknowledgements, and the deadline.

NINA uses the existing native accessory optical guard and preserves its filter
configuration. The shared ASCOM helper retains independent client leases within
one application. Alpaca reserves 0 for EFW and 1 for standalone QHY; the new,
unreleased camera bridge entries use camera slot + 2, preserving each slot's
UUID. Neither standalone USB nor the camera bridge requires a new executable.

Software validation: 23 Alpaca tests and 7 QHY tests passed; the NINA suite
passed 694 tests (6 hardware/environment tests skipped). Five shared recovery
scenarios now run for both EFW and USB CFW: stale handle, changed position,
lost move acknowledgement, explicit disconnect, and multiple client leases.
The ASCOM registration project builds successfully; installer scripts parse.
Machine-wide installation remains for the disposable CI installer gate.


### Completed standalone USB acceptance

- `usb-sweep-02`: traced raw protocol, all nine slots ascending/descending,
  wraparound, long jumps and same-slot commands, returning to position 0.
- `usb-ascom-01`: production Rust worker through native IFilterWheelV2; complete
  sweep plus long/same-slot moves. Two ASCOM objects shared the port, releasing
  either lease did not disconnect the other, and reopening preserved position.
  Adjacent moves settled in about 1.6 s; four-slot jumps in about 5.2 s.
- `usb-alpaca-direct-01`, `usb-alpaca-sdk-01`: standalone USB wheel plus QHY600M
  camera through HTTP; all nine slots and four full five-second ImageBytes
  captures per camera backend, with independent camera/wheel reconnects.
- `installed-nina-usb-01`: real installed NINA 3.2, with peripheral optical
  verification enabled. Both camera backends passed all nine slots, camera-only
  disconnection with wheel motion, camera reconnection, and wheel reconnection.
  Ten full 9600 x 6422 RAW16 FITS files were independently verified for distinct
  pixel hashes, QHY600M model, CAMSER, and the expected FILTER values.

The first installed-NINA SDK attempt lost its worker after a logged
253,053,600-byte allocation failure during a concurrent full-workspace Rust
build. No frame was accepted for that failed exposure. The entire SDK pass
was rerun without the concurrent build (`usb-sdk-rerun-*`) and all five full
frames passed. Preserve the original failure and its diagnostic batch alongside
the passing run; do not classify it as a serial-wheel failure. No physical
cable removal, firmware update, reset or calibration was performed.

After USB acceptance, original installed NINA binaries and Regain settings
were restored and hash-verified. The temporary profile and opt-in fixture were
archived, both devices disconnected, and the wheel left at position 0.
