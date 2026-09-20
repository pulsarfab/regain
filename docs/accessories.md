# Native EFW and EAF drivers

PulsarFab regain's EFW filter-wheel and EAF focuser support uses the `regain-accessories`
Rust library and worker. Windows ASCOM, the native NINA providers, and Alpaca
all use this worker. Neither ZWO accessory SDK nor an Alpaca bridge is needed
for local Windows operation.

## Verified hardware and scope

On 2026-09-18 the USB trace and hardware validation used:

| Device | USB VID:PID | Firmware | Reported model | Verified behavior |
| --- | --- | --- | --- | --- |
| Seven-position EFW | `03c3:1f01` | 3.6.2 | EFW-S-0 | All seven slots, position, serial, direction command encoding, SDK and native calibration |
| Unmounted EAF | `03c3:1f10` | 3.8.1 | EAFN | Position, short moves both ways, halt, temperature, beep, reverse, backlash, travel limit |

The wheel started and finished at zero-based position 0 (displayed slot 1).
The focuser started and finished at 39,829 steps, with a 60,000-step limit,
beep enabled, reverse disabled, and zero hardware backlash. Tests restored
these values. They did not perform a full focuser travel sweep or load test.

The EAF parser requires EAFN firmware 3.3.6 or newer, which uses the 24-bit
position format. Only the listed firmware has been hardware-tested. Earlier
16-bit EAF firmware, EAF Pro/Bluetooth, dual-disc EFWs, coordinate
reset, alias writes, firmware updates, and USB resets are not exposed. No
calibration or reference reset happens automatically during connection.

## Windows and NINA setup

Build/install the normal PulsarFab regain package. The NINA plugin adds **PulsarFab regain EFW
Filter Wheel** and **PulsarFab regain EAF Focuser** to their respective equipment lists.
The ASCOM installer registers `ASCOM.ZWOgain.FilterWheel` (IFilterWheelV2) and
`ASCOM.ZWOgain.Focuser` (IFocuserV3) for both 32-bit and 64-bit clients.

Open Setup from the equipment gear or ASCOM Chooser. The shared WPF dialog
uses the same theme as the CAA: host colors in NINA, a light standalone theme.
Select a serial on **Device**, then connect for setup. A missing saved serial
never falls back to another device. With no saved selection, the only available
device can be selected and saved on connection.

- **EFW:** Motion shows slots 1–N; the ASCOM/NINA interfaces use 0–N−1 and
  return −1 while moving. Filters provides names, focus offsets, and the
  unidirectional movement option. At least one offset must be zero. NINA keeps
  its existing filter exposure/autofocus settings and initializes missing slots
  from the driver defaults; edit those NINA profile settings in NINA afterward.
  **Motion → Calibrate wheel** rotates the wheel to detect its slots and finishes
  at displayed slot 1. Calibration takes about 50 seconds on the tested wheel.
  Progress remains visible and movement is unavailable until completion.
- **EAF:** Motion accepts an absolute step position and provides Halt. Settings
  exposes beep, reverse, 0–255 steps of hardware backlash, and a maximum travel
  limit up to 600,000. The limit cannot be below the current position. Changes
  require idle hardware and are read back. Use zero hardware backlash when the
  imaging application supplies backlash compensation.

The focuser reports absolute positioning. Its microns per step depend on the
attached mechanics, so ASCOM StepSize is not implemented. TempCompAvailable is
false; use the imaging application's compensation. Temperature is reported
when valid. These behaviors follow the [ASCOM focuser interface](https://ascom-standards.org/newdocs/focuser.html).
Filter names, focus offsets, and moving-position behavior follow the
[ASCOM filter-wheel interface](https://ascom-standards.org/newdocs/filterwheel.html).

Only one controller owns a physical USB device. Do not connect native NINA,
native ASCOM, the vendor driver, and Alpaca to the same device simultaneously.
Multiple Alpaca clients can share the server-owned connection by ClientID.

Profiles are stored in `%LOCALAPPDATA%\Regain\Accessories\`:
`efw-nina.json`, `eaf-nina.json`, `efw-ascom.json`, and `eaf-ascom.json`.
Diagnostic logs are `efw.log` and `eaf.log` in that directory. Device settings
such as EAF reverse/backlash live in the device; filter names/offsets and
unidirectional preference live in the frontend's profile.

For development, `REGAIN_ACCESSORY_WORKER` overrides the native frontend worker path,
`REGAIN_ACCESSORY_SETTINGS` overrides the profile directory, and
`REGAIN_ACCESSORY_SIMULATE=1` explicitly enables simulated native sessions.
Simulation is never an automatic hardware fallback.

## Alpaca setup

Start `regain-alpaca` as described in the main README. On its setup page choose
**EFW filter wheel setup** or **EAF focuser setup**, discover devices, select the
serial, and connect for setup. Disconnect the setup client after editing.

| Device | Setup page | Alpaca API |
| --- | --- | --- |
| EFW | `/setup/v1/filterwheel/0/setup` | `/api/v1/filterwheel/0/` |
| EAF | `/setup/v1/focuser/0/setup` | `/api/v1/focuser/0/` |

Each type exposes device number 0 and appears in management discovery after
selection. With `--profiles cameras.json`, accessory profiles are
`cameras.efw.json` and `cameras.eaf.json`. UUIDs remain stable when profiles are
edited. Both expose read-only `Regain.Status` and `Regain.Identity` actions.
The EFW also exposes `Regain.Calibrate` and a **Calibrate wheel** button.
The EAF web page supports the same hardware settings as the WPF dialog.

![EFW Alpaca setup with simulated seven-slot wheel](images/alpaca-efw.png)

![EAF Alpaca setup with simulated focuser](images/alpaca-eaf.png)

## USB tracing playbook

The SDK ZIPs are research inputs kept under ignored `.reference/`, not shipped.
The probe launches its own Python process, loads the selected DLL, waits for
the parent to attach Frida to that process, then starts SDK calls. It derives
HID import locations from the PE import table and records each SDK call and
HID report. It never attaches to NINA or another user's process.

Inputs:

| SDK | DLL SHA-256 |
| --- | --- |
| EFW 1.8.4 x64 | `be05a25d5d25149876e7bcce680b4520e36091b8b60b23ab6a3149c1765f99aa` |
| EAF 1.8.1 x64 (Windows) | `9272a27ead2887bdd555326af797b6c88e5f034aa3b6c34fad57d6ac9904c00e` |

Use the environment and dependencies from [inspection setup](../scripts/inspection/README.md):

```powershell
.reference/inspection-venv/Scripts/python.exe scripts/inspection/accessory_sdk_probe.py efw --output artifacts/inspection/efw-readonly.jsonl
.reference/inspection-venv/Scripts/python.exe scripts/inspection/accessory_sdk_probe.py eaf --output artifacts/inspection/eaf-readonly.jsonl
# Only with a clear wheel and an unmounted or mechanically safe focuser:
.reference/inspection-venv/Scripts/python.exe scripts/inspection/accessory_sdk_probe.py efw --exercise --output artifacts/inspection/efw-exercise.jsonl
.reference/inspection-venv/Scripts/python.exe scripts/inspection/accessory_sdk_probe.py eaf --exercise --output artifacts/inspection/eaf-exercise.jsonl
# EFW calibration followed by a full slot sweep:
.reference/inspection-venv/Scripts/python.exe scripts/inspection/accessory_sdk_probe.py efw --calibrate --exercise --output artifacts/inspection/efw-calibrate.jsonl
```

The default probe performs read-only API calls, but **EAFClose itself emits a
stop report**. Exercise mode visits all wheel slots or makes a 50-step EAF round
trip, probes settings, and restores the originals. Use a fresh output filename;
the probe refuses to overwrite evidence. Raw traces contain serial numbers and
remain in ignored `artifacts/`. [Reviewed evidence](accessory-evidence.json)
contains only selected packets, hashes, and outcomes.

`--calibrate` is EFW-only. It requires an idle wheel with no hardware error,
calls `EFWCalibrate` once, and waits up to 90 seconds for moving followed by
idle. It checks that the slot count and direction preference are unchanged,
then restores the starting position. On an uncertain result it does not retry
calibration or send a restoration move while calibration may still be active.
Adding `--exercise` checks every slot afterward. This probe uses the SDK for
comparison; the production calibration command uses the native Rust driver.

### Framing and identity

Offsets include HID report ID byte zero. All commands begin `03 7e 5a`.
Queries are `03 7e 5a 02 SELECTOR`, zero-padded to the HID descriptor's output
length. Replies begin `01 7e 5a SELECTOR`. The attached EFW has 64-byte output
and 16-byte input reports; the EAF uses 16 bytes in both directions.
Selector 4 returns firmware at bytes 4–6 and model text at 8–15. Selector 12
returns the eight-byte serial at 4–11. EFW identity queries require the SDK's
200 ms settling interval. Every response header and length is checked.

### EFW protocol

Query selector 1 returns wheel state at byte 4: 1 is idle, 6 is hardware error,
and non-idle states report motion. Byte 5 is the error code when state is 6.
Bytes 6, 7, and 8 must agree for a settled single-disc position. Their value
is one-based; byte 9 is the slot count. Byte 12 identifies the secondary disc
in the SDK's dual-disc path, which this implementation rejects.

Move output: `03 7e 5a 01 MODE SLOT`, with MODE 2 for normal movement or 3 for
unidirectional movement and SLOT one-based. The SDK's direction setter changes
host state; the next move embeds the mode. The initial exercise observed normal
moves; the mode-3 encoding was verified in SDK 1.8.4 x64 code at RVA 0x1899–0x18c4.
The driver does not copy the SDK's automatic retry/recalibration paths.

### EFW calibration trace

The attached seven-position wheel completed SDK calibration in **48.766 seconds**.
The trace contains exactly one calibration write, `03 7e 5a 01 01`, zero-padded
to 64 bytes. This also matches SDK 1.8.4 x64 `EFWCalibrate` at RVA
0x39f2–0x3a25. During calibration, status byte 4 was 0 and `EFWGetPosition`
returned −1. Byte 9 progressed through counts 1–7; these are provisional counts
while the wheel detects slots, not a changed filter configuration.

Completion returned state 1, seven slots, all three position sensors at 1,
and hardware error 0. The wheel returned to displayed slot 1 (zero-based 0),
with the original direction preference. A subsequent SDK sweep reached every
slot, followed by an independent native Rust sweep; both restored slot 1.
All captured HID operations succeeded. The raw trace hash and selected reports
are recorded in [reviewed evidence](accessory-evidence.json).

### Native calibration controls and API

In native ASCOM or NINA setup, connect the wheel and choose **Motion → Calibrate
wheel**. In Alpaca, use **Calibrate wheel** on the filter-wheel setup page.
The CAA-style native dialog displays live calibration progress:

![Native EFW calibration controls in simulation](images/native-efw-calibration.png)

All three frontends advertise `Regain.Calibrate` in `SupportedActions`. Call
`Action("Regain.Calibrate", "")` in native ASCOM/NINA, or PUT to
`/api/v1/filterwheel/0/action` with `Action=Regain.Calibrate`, empty `Parameters`,
and the connected `ClientID` for Alpaca. The action returns the string `null`
after accepting the command. It does not wait for physical completion.

Poll `Regain.Status`: `calibrating` is true while active, `detected_slots` is
the provisional count, and `slots` retains the original count. `Position`
returns −1 throughout calibration. Completion requires observed movement
followed by idle, the original slot count, no hardware error, and slot 1.
Filter names, focus offsets, and the direction preference are preserved.
The attached wheel passed calibration through the Rust worker (48.640 s),
64-bit ASCOM (48.624 s), 32-bit ASCOM (48.541 s), and Alpaca (48.625 s).
Each hardware test returned the wheel to slot 1 and verified subsequent movement.
NINA action/profile tests and both setup-button tests passed with simulation.

Calibration requires idle hardware. Duplicate calibration and movement commands
are rejected while it is pending. USB failure, a changed final slot count, or
a 90-second timeout sets a motion fault; inspect the wheel before reconnecting.
The driver never repeats an uncertain calibration write. The wheel has no
verified halt command: closing setup or disconnecting does not stop it.
Connecting during wheel movement is rejected so provisional slot counts cannot
overwrite filter metadata. Calibration is never automatic during connection.

### EAF protocol

Query selector 3 returns:

| Bytes | Meaning on EAFN firmware 3.3.6+ |
| --- | --- |
| 4 | Motion state (0 idle, 1 moving) |
| 5 | Hardware backlash |
| 7–9 | Big-endian 24-bit current position |
| 6, 14, 15 | Big-endian 24-bit maximum position |
| 11–12 | Big-endian temperature: raw / 100 − 300 °C |
| 13 | Bit 0 beep, bit 1 reverse, bit 2 hand control; high nibble error |

For example `01 7e 5a 03 00 00 00 00 9b 95 00 80 52 01 ea 60`
decodes to position 39,829, limit 60,000, temperature 28.5 °C, and beep enabled.
The firmware-dependent position decoder is at SDK RVA 0x256dd–0x25773; direct
temperature conversion at 0x25a21–0x25a3a.

Writes use command 3 and preserve the current backlash, travel limit, and
beep/reverse settings. Byte 4 is 1 for move, 0 for halt/settings. Position is
at 7–9; byte 10 is 2 when applying the limit. Temperature bytes are zeroed.
The observed move from 39,829 to 39,879 is
`03 7e 5a 03 01 00 00 00 9b c7 00 00 00 01 ea 60`.

## Native implementation and validation

`regain-hid` shares the CAA's native Windows HID, Linux hidraw, and macOS
IOKit transports. Windows and macOS request exclusive device access; Linux
uses an advisory lock, so other non-cooperating drivers must still be closed.
On Linux grant the current user access to HID devices with product IDs 1f01
and 1f10 using a local udev rule, then replug them. No Zadig/libusb replacement
driver is required for these HID accessories.

`regain-accessories` validates packets, serial selection, slot/step ranges,
and settings readback. The worker accepts one JSON request per line and returns
`{"ok":true,"result":...}` or `{"ok":false,"error":"..."}`. Commands are
`identity`, `status`, `move`, `calibrate` (EFW), `halt` (EAF), and `settings` (EAF). It polls active
motion, limits EFW operations to 60 seconds and EAF operations to 600 seconds,
halts the EAF on timeout/fault/EOF, and never replays uncertain motion writes.
Halt waits up to one second for reported idle after issuing exactly one stop
write. An active-motion hardware test stopped at 39,839 during a move from
39,829 toward 39,879, then returned to 39,829.
The wheel has no verified halt command; disconnecting cannot stop an ongoing
wheel movement. Errors require inspection and reconnection before another move.

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
python scripts/validate_accessories.py
python scripts/test-alpaca-accessories.py
./scripts/test-accessory-ascom.ps1
# Explicit hardware tests (short EAF movements; keep mechanics clear):
python scripts/validate_accessories.py --hardware --exercise
python scripts/test-alpaca-accessories.py --hardware
./scripts/test-accessory-ascom.ps1 -Hardware
# Explicit EFW calibration tests; each calibration finishes at slot 1,
# and the test restores its original position after checking completion:
python scripts/validate_accessories.py --hardware --exercise --device efw --calibrate
python scripts/test-alpaca-accessories.py --hardware --calibrate
./scripts/test-accessory-ascom.ps1 -Hardware -Calibrate
```

The listed hardware passed native-worker, Alpaca, and both 32-bit/64-bit COM
round trips. Unit tests cover captured packets, malformed/stale data, sensor
disagreement, bounds, preserved settings, and non-retry of uncertain writes.
Calibration tests cover provisional counts, stale idle reports, changed final
slot count, hardware faults, timeout, duplicate commands, and preserved metadata.
NINA providers compile against NINA 3.2.0.9001, and integration tests exercise
focuser movement/cancellation and filter-profile preservation; an interactive NINA autofocus
sequence and ASCOM ConformU certification have not been run. Linux/macOS
hardware validation remains outstanding.
