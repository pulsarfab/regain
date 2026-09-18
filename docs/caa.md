# Native CAA driver

`zwogain-caa` is a Rust library and command-line driver for the ZWO CAA rotator.
It uses USB HID directly, with no ZWO SDK, hidapi C library or libusb dependency.
It is independent of the camera driver. The NINA plugin and Windows ASCOM driver
use this worker; see [frontend setup and actions](caa-frontends.md). Alpaca rotator
support uses the same worker. ZWOgain is not affiliated with ZWO.

## Run

CI builds standalone CAA ZIPs for Windows x64, Linux x64/ARM64 and macOS
Intel/ARM64. Extract the ZIP and run the binary. No SDK installation is needed.
From source:

```powershell
cargo build -p zwogain-caa --release --locked
target/release/zwogain-caa.exe list
target/release/zwogain-caa.exe status
# Requires clearance for motion within eight degrees of the starting position:
target/release/zwogain-caa.exe exercise
```

On Linux/macOS omit `.exe`. If several CAAs are attached, use `--path` with a
path returned by `list`. `status` reads identity, settings and position. `list-details` adds model, firmware
and serial; `--serial SERIAL` selects a stable identity across USB reattachment.
`exercise` tests motion, logical sync, reverse, beep, limit and stop, then
returns to the initial mechanical position and restores settings. Do not run
another rotator controller at the same time.

`serve` accepts one JSON object per line and returns one JSON response. Keep
the process alive to retain logical sync across commands:

```json
{"command":"status"}
{"command":"move-mechanical","degrees":154}
{"command":"stop"}
{"command":"sync","degrees":123.45}
{"command":"move-to","degrees":124.45}
{"command":"move-relative","degrees":-1}
{"command":"beep","enabled":false}
{"command":"reverse","enabled":true}
{"command":"limit","degrees":360}
{"command":"alias","text":"MyCAA"}
{"command":"identity"}
{"command":"settings"}
```

Motion commands return after submission; poll `status` until idle and check
position and `error`. The library also offers `wait_for(target, timeout)`,
which attempts a stop on a fault or deadline. Uncertain writes are never
replayed automatically. Position/settings can be reread after a failed call.
An uncertain reverse write is reconciled before using logical coordinates.

Logical sync is local to a connection. Applications should persist their
coordinate offset against the device serial and restore it on reconnect.
Beep, reverse, limit and alias are device settings. Releasing a connection
does not send motion commands; explicitly stop first when that is wanted.

## USB transport

- VID/PID: `03c3:1f20`, vendor-defined HID.
- Tested CAA-M54: 16-byte input and output reports, including report ID.
- Output: HID report ID 3, sent through SET_REPORT(Output).
- Input: HID report ID 1, fetched through GET_REPORT(Input).
- Windows: SetupAPI, `HidD_SetOutputReport` and `HidD_GetInputReport`.
- Linux: hidraw `HIDIOCSOUTPUT`/`HIDIOCGINPUT`; report sizes come from the HID
  descriptor. No detach or replacement of the kernel HID driver is needed.
- macOS: IOKit `IOHIDDeviceSetReport`/`IOHIDDeviceGetReport`.

Windows/macOS request exclusive access to the selected CAA. Linux uses an
advisory `flock`; the vendor SDK does not honor that lock. Linux users need
hidraw permissions, for example a udev rule granting the local astronomy
group access to VID `03c3`, PID `1f20`. The implementation targets the Linux
x64/ARM64 ioctl layout used in CI.

The driver serializes request/reply pairs, checks report lengths and echoed
command headers, and zeroes unused output bytes. Metadata reads and writes
retain the SDK's 200 ms delay. Status/settings queries need no added delay.
OS HID calls are synchronous: a library wait deadline is checked between
calls and cannot interrupt a blocked OS call. Use a supervised process when
a hard deadline is required.

## Protocol

Byte offsets below include the HID report ID. Integers are big-endian.
All output reports start `03 7e 5a`; all input reports start `01 7e 5a`.
Query reports contain `02 selector` at bytes 3–4. Replies echo the selector
at byte 3. There is no observed transaction number or checksum.

| Query selector | Reply data |
| --- | --- |
| `03` | Status, position, temperature ADC, limit and error (below) |
| `04` | Firmware bytes 4–6; model text starts at byte 8 |
| `08` | Beep at byte 4, reverse at byte 5 |
| `0c` | Eight-byte serial at bytes 4–11 |
| `0d` | Eight-byte alias at bytes 4–11 |

Status reply:

| Bytes | Meaning |
| --- | --- |
| 4 | State: observed 0 idle, 1 moving; other values remain exposed as raw state |
| 5 | Direction field, preserved in motion writes |
| 6–9 | Mechanical angle in units of 0.0001 degree |
| 10 | Unresolved/reserved |
| 11–12 | Temperature ADC |
| 13–14 | Whole-degree rotation limit |
| 15 | Fault: SDK maps 1 general, 2 stall, 3 timeout, 4 over-limit |

Movement and settings:

| Output bytes | Operation |
| --- | --- |
| `03 7e 5a 03 01 direction position32 ... limit16` | Mechanical move; position at 6–9, limit at 14–15 |
| `03 7e 5a 03 02` | Stop |
| `03 7e 5a 03 00 direction 0b b8 00 00 02 00 00 00 limit16` | Set limit; matches SDK default speed 3000 and update selector 2 |
| `03 7e 5a 07 boolean` | Set beep |
| `03 7e 5a 09 boolean` | Set reverse |
| `03 7e 5a 0d alias[8]` | Set alias |

The SDK implements relative/logical moves by converting them to mechanical
targets. Direction reversal changes this conversion and preserves the logical
angle using an offset. Mechanical 0 and 360 are retained as distinct endpoints.
The protocol's angle units are finer than physical resolution: the
[CAA manual](https://i.zwoastro.com/wp-content/uploads/2025/01/dfe48610abdd30aa57a31b0dd0431de8.pdf)
specifies 0.02° resolution and 0.1° positioning accuracy.

Important SDK differences:

- `CAACurDegree(0)` sends a mechanical-reference reset. Other values adjust
  a host-side offset. Rust `sync`, including zero, always changes only the
  logical offset; it preserves the cable-limit reference.
- `CAAMinDegree` is declared in the header but has no matching C export in
  this DLL. A decorated symbol at the same address as `CAACurDegree` uses a
  float argument, contrary to the header's pointer declaration. It is unused.
- SDK close sends stop/configuration writes. Rust close simply releases the
  handle. Firmware update and undocumented speed controls remain outside this driver's API.
  Explicit mechanical-reference writes are available separately from sync.
- Temperature uses the SDK's NTC resistance table and interpolation. Invalid
  or out-of-table ADC readings return `None`; no cached substitute is returned.

## Mechanical reference and travel limits

Validated on the CAA-M54, firmware 1.1.1:

- The device owns the mechanical position. Setting its reference from 152 to
  153 degrees survived closing and reopening the native HID handle.
- SDK `CAACurDegree(id, 0)` really resets that reference: the native driver
  read zero after the SDK closed. It is different from ordinary logical sync.
- The raw reference-update report accepts nonzero positions too. We restored
  152 degrees without requesting motor movement. Use command `03`, action
  `00`, reference units at bytes 6–9, update selector `01` at byte 10, and the
  current limit at bytes 14–15. Preserve direction at byte 5 and zero reserved
  bytes. Reference units are big-endian, 10,000 per degree.
- A raw 361-degree limit was accepted. Starting from a temporarily assigned
  360-degree reference, a target of 361 reached 360.99 degrees with no fault.
  This first test crossed the numeric 360 boundary with a short move.
- A separate test completed five successive 90-degree forward moves, resetting
  mechanical zero before each: 450 degrees of cumulative reported travel with
  the limit left at 360. Five reverse moves unwound that travel. Every leg
  reached its target with no fault. The final reported displacement was zero,
  and the original 152-degree reference, 360-degree limit and settings were
  restored. Zero resets therefore bypass the cumulative one-turn restriction.
  This does not establish unlimited operation or independently measured accuracy.
- With the limit still at 360, requesting 361 started moving **backward** from
  360. The probe stopped it at 359.15. This is consistent with wrapping the
  target, but we did not let it finish to establish its destination. Firmware
  did not return an error. Do not rely on it to reject an out-of-range move.

The SDK rejects mechanical targets below zero or above 360 with error 10,
even though the USB protocol can exceed that range. The production Rust API
supports the validated 0–361 mechanical range and 1–361 limit. Logical angles
stay within one turn. Every move is checked against the device limit before
USB submission. See [reference-test results](caa-reference-evidence.json).
Changing the mechanical reference also changes the origin for travel limits.
Normal connection, recovery and logical sync must not reset it automatically.

The reference and motion tests restored the original reported position (152)
and limit (360). Displacement was tracked from device reports; there was no
independent angle measurement. Power-loss retention remains untested: its
temporary marker was restored before the multi-turn test, with no confirmed
power cycle. The SDK cannot tell us whether firmware stores position in flash, EEPROM, or
another mechanism, nor whether it uses an encoder.

The Windows-only `scripts/inspection/validate_caa_reference.py` records raw
reports and checks model, firmware and report lengths. Its `assign`,
`sdk-zero` and `boundary` phases exercise the cases above. Motion has a
five-second polling deadline, stops on opposite travel or excessive reported
displacement, and attempts reference/limit/position restoration. OS HID calls
themselves are synchronous. Provide clearance for eight degrees; only one
CAA may be connected. The `sdk-zero` phase requires the hash-pinned DLL.

For a physical power test, run `prepare-power`, remove USB power for at least
five seconds without turning the rotator, then run `finish-power`. Each phase
requires a new `--output` JSONL path. The shared `--state` file saves the
original reference and device fingerprint before setting the temporary marker.
The final phase checks identity, reports retention and restores the reference.

`scripts/inspection/validate_caa_multiturn.py --output artifacts/NEW-caa-turns.jsonl`
is a separate, larger-travel experiment: five forward 90-degree moves with a
zero reset before each, followed by five reverse moves. It requires clearance
and cable slack for 450 degrees of travel. Each leg checks position, direction,
faults and a 30-second deadline. It stops on failure and records the remaining
reported displacement rather than automatically unwinding from an uncertain
state. On success it restores the original reference and checks settings.

## Workup, 2026-09-17

Inspected Windows CAA SDK 1.5.9 x64, SHA-256
`413d629adfd81150962211d2241aec2a99420ba2d65c0161f7700da32d534152`.
Relevant RVAs: motion `1f60/23d0`, status `1ae0/2760`, settings `25b0`,
temperature `2860/29e0`, logical angle `2df0`, sync `2040`, limit `2140/21e0`.
The SDK has an MIT-style license; the NTC table's notice is retained in the crate.
`scripts/inspection/extract_caa_temperature.py` reproduces that table from
the hash-pinned DLL.

The attached CAA-M54 reports firmware 1.1.1. Initial state: 152°, idle,
beep on, reverse off, 360° limit. Windows initially showed it disconnected;
reseating restored enumeration in both Windows and the SDK.

Passed on real hardware:

- SDK movement 152→154→152°, beep/reverse round trips and limit write.
- Native mechanical, relative and logical moves; logical sync and reversed
  relative motion; beep and limit round trips; out-of-limit request rejection.
- Stop during motion, before the target; return to the original 152°.
- Alias write/read/restore; close/reopen and SDK cross-check afterward.
- A forced failed status-read return, followed by a successful fresh read.
- A forced failed movement-write return after hardware accepted the command:
  exactly one motion command, position reached, no automatic replay.
- Native process module inspection: no CAA/ASI SDK loaded.
- A second native process was refused while the first held the device.

See [sanitized results and local trace hashes](caa-evidence.json).

Local validation passed 15 CAA regression tests, the full Rust workspace tests,
Clippy, a source-only Cargo package build and standalone ZIP checks. The
[initial CI run](https://github.com/theatrus/zwogain/actions/runs/35300657756)
passed builds/tests on Linux x64/ARM64 and macOS Intel/ARM64. Windows hardware
results above use the local build; the full Windows packaging job is separate.

Both SDK and native report an unavailable temperature sensor (ADC 0; SDK
error 7). Temperature conversion has synthetic regression tests, but a real
probe is not validated. Hand-controller behavior, physical stall, cable
removal, power cycling, and Linux/macOS hardware remain
untested. Serial numbers and raw traces stay in local ignored artifacts.

Reproduce the traced Windows workup:

```powershell
python scripts/inspection/caa_sdk_probe.py --exercise --output artifacts/NEW-caa-sdk.jsonl
python scripts/inspection/validate_caa.py --output artifacts/NEW-caa-native.jsonl
```

These scripts need `scripts/inspection/requirements.txt`. The first also
needs the SDK in `CAA_Windows_SDK_V1.5.9/` (ignored), or an explicit `--sdk`.
It pins the inspected DLL hash before attaching version-specific hooks.
The second uses only the compiled native worker and Frida. Its two faults
alter HID call results after real transfers; they are not physical USB faults.

## Frontend worker protocol

NINA and the Windows ASCOM driver use `serve --serial SERIAL`. Commands and
replies are newline-delimited JSON. The worker ticks motion independently of
requests, so clients do not have to poll to advance a segmented move. EOF halts
motion and cancels queued segments; closing a bare library handle still just
releases it. No motion command is retried.

New commands: `reference` with `degrees` (0–360), `reset-origin` with no fields,
and `rotate-unwrapped` with physical `degrees` (-450..450, nonzero). `limit`
accepts 1–361. `status` includes `logical_offset`, `target_degrees`, and
`motion_error` alongside the device fields. `stop` discards pending segments
before advancing anything and acknowledges stored motion errors. Logical sync
never calls the mechanical-reference command.

See [frontend setup and ASCOM actions](caa-frontends.md) for persistence,
coordinate conventions, limits, and failure handling.
