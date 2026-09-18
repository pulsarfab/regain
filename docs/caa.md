# Native CAA driver

`zwogain-caa` is a Rust library and command-line driver for the ZWO CAA rotator.
It uses USB HID directly, with no ZWO SDK, hidapi C library or libusb dependency.
It is independent of the camera driver. There is no NINA/ASCOM/Alpaca rotator
frontend yet. ZWOgain is not affiliated with ZWO.

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
path returned by `list`. `status` reads identity, settings and position.
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
  handle. Firmware update, mechanical-reference reset and undocumented speed
  controls are deliberately outside this driver's API.
- Temperature uses the SDK's NTC resistance table and interpolation. Invalid
  or out-of-table ADC readings return `None`; no cached substitute is returned.

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

Both SDK and native report an unavailable temperature sensor (ADC 0; SDK
error 7). Temperature conversion has synthetic regression tests, but a real
probe is not validated. Hand-controller behavior, physical stall, cable
removal, power cycling, full-turn travel, and Linux/macOS hardware remain
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
