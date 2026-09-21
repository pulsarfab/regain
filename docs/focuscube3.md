# Pegasus Astro FocusCube3

`regain-pegasus::fc3` is an independent Rust library, exposed through `regain-device pegasus fc3`, that speaks directly to
the FocusCube3 USB serial port. It requires neither Pegasus Unity nor an ASCOM
driver. Native NINA, native Windows ASCOM, and Alpaca use this same crate.
This support is included from release 0.4.0.0.

## Windows setup

Connect USB and close/disconnect Pegasus Unity's device connection. Unity's
background `Peg.Server.exe` can retain the port after its window closes.
Windows uses the built-in USB Serial Device driver; do not install WinUSB/Zadig.
Selection uses the USB serial number, not the current COM number.

* **Native NINA:** install the PulsarFab regain plugin, choose **PulsarFab regain Pegasus
  FocusCube3** in the focuser list, and open the setup gear. Refresh, select the
  device, connect, and check its position and settings. The worker must be
  beside the plugin DLLs. Requires NINA 3.2.0.9001 or later on Windows x64.
* **Native ASCOM:** install the PulsarFab regain ASCOM installer (Windows x64, .NET
  Framework 4.8, ASCOM Platform). Choose **PulsarFab regain Pegasus FocusCube3** in the
  Focuser Chooser. Its ProgID is `ASCOM.ZWOgain.FocusCube3.Focuser`. The Start
  menu's **FocusCube3 setup** opens the same styled dialog through the shared
  server. Both 32-bit and 64-bit clients are supported. Alpaca is not required.
* **Alpaca:** start `regain-alpaca --port 11111` and open
  `http://127.0.0.1:11111/setup/v1/focuser/1/setup`. Find/select the device,
  connect for setup, then disconnect setup before closing the page. Select
  **PulsarFab regain Pegasus FocusCube3**, Focuser device **1**, in the client. EAF
  remains Focuser device 0. Only a configured FC3 appears in management discovery.

The native setup dialog uses the same WPF theme as the CAA/EAF drivers, and
inherits NINA's theme when hosted there. These screenshots use the attached
physical device, firmware 1.8.2; neither uses simulation.

![Physical FocusCube3 native setup](images/native-fc3.png)
![Physical FocusCube3 Alpaca setup](images/alpaca-fc3.png)

Positions are absolute steps, with a driver range of 0–1,000,000. This is a
software range, not a measured mechanical limit. Hardware backlash is
0–1,000 steps; set it to zero when NINA handles backlash. Motor speed accepts
**even integers from 2 through 400**. Firmware 1.8.2 rounds odd values down,
including 1 to 0. A device reporting speed 0 can connect for repair but cannot
move until a valid speed is set. Direction reversal and backlash are hardware
settings. Microns per step and temperature compensation are left to the host
application; an absent/out-of-range temperature probe reports unavailable.

Native profiles live in `%LOCALAPPDATA%\Regain\Accessories\fc3-nina.json`
and `fc3-ascom.json`. Alpaca stores `cameras.fc3.json` beside `cameras.json`
(or the equivalent name beside `--profiles`). The saved Alpaca UUID survives
profile edits.

## Port ownership and deferred sharing

USB serial ports have one owner. Native NINA, Alpaca, and Pegasus Unity cannot
connect directly at the same time. A busy/missing device fails connection;
the driver does not silently switch devices or fall back to a simulator.

**ASCOM clients share one out-of-process COM server**,
`Regain.FocusCube.ASCOM.exe`, and one serial worker. Each COM object owns its
own connection lease. Disconnecting one client leaves the others connected;
the last disconnect releases the port. Setup retains the connection while its
dialog is open. COM releases from exited clients are collected by the server;
the server exits after its remaining COM objects and locks have been idle for
about 30 seconds. All ASCOM clients see and control the same focuser position.

Alpaca clients likewise share its worker using distinct ClientIDs. Broader
sharing between the **native NINA plugin, ASCOM server, Alpaca, and Unity** is
deliberately deferred. Pegasus's background server supplies that role for its
own platform. A future common broker could own the serial connection and
arbitrate these frontends; this implementation does not create such a service.
NINA can use our ASCOM driver when it needs to share with other ASCOM clients.

On Linux, grant access to `/dev/ttyACM*` (often via `dialout`); on macOS use
the enumerated `/dev/cu.usbmodem*` port. Neither requires a vendor SDK.

## Protocol and tracing

Sources: Pegasus's [FocusCube3 command list](https://pegasusastro.com/command-list-for-focuscube3/),
local inspection of the installed Unity3 `Peg.Engine.dll` and its FocusCube3
driver, and application-level serial traces from the physical revision B device.
Vendor DLL SHA-256:
`75a3232e12dfb401f424d80680c10428c0db02985e29d4d9313de0b224fbcbec`.
No vendor implementation is bundled. The trace is serial request/reply data,
not a claim of a USB bus capture.

USB VID/PID is `303a:9000` (ESP32-S3 CDC; shared with other devices), 115200 baud,
8 data bits, no parity, one stop bit, no flow control, DTR asserted. Requests
end in LF; replies end in LF with optional CR. `F#` verifies the model before
accepting a candidate. Firmware 1.8.2 sends **bare reply values**, unlike the
prefixes shown for several commands in the published table. The parser accepts
the documented prefixed form as well, but only the bare form was hardware-tested.

| Request | Observed reply | Meaning |
| --- | --- | --- |
| `F#` | `FC3_<id>_B` | Identity and hardware revision |
| `FV` | `1.8.2` | Firmware version |
| `FA` | `FC3:1250:0:27.7:0:0` | Position, moving, °C, reverse, backlash |
| `FP`, `FI`, `FT`, `FU` | Bare integer/decimal | Position, moving, temperature, uptime (probe only) |
| `FM:1300` | `1300` | Start absolute movement |
| `FH` | `1` | Request halt; continue polling until idle |
| `SP` / `SP:400` | `400` | Read/set speed |
| `SP:399` / `SP:1` | `398` / `0` | Observed even-speed quantization |
| `BL:5` | `5` | Set hardware backlash |
| `FD:1` | `1` | Reverse motor direction |

Relative motion, coordinate sync, reboot/reset, Wi-Fi control, and credential
commands are not exposed. In particular no Wi-Fi password commands are queried.

The transport bounds frames to 256 bytes and a two-second response deadline.
It never automatically replays uncertain writes. Communication/malformed-reply
faults latch until reconnect, avoiding consumption of a late reply as a new
command's response. The worker polls pending motion every 250 ms; premature
idle is an error. A ten-minute motion deadline sends one halt on a synchronized
connection and latches a fault. Halt waits up to five seconds for deceleration.
Clean worker EOF halts pending motion when the transport is still healthy.
JSON stdin is bounded to 4 KiB per request and accepts the .NET Framework BOM.

## Reproduce validation

```powershell
cargo build --workspace --locked
cargo test -p regain-pegasus
python scripts/test-fc3.py
./scripts/test-fc3-ascom.ps1

# Physical device; only use with clearance for short moves in both directions.
./scripts/inspection/fc3_probe.ps1 -Port COM6 -Move
python scripts/test-fc3.py --hardware --serial YOUR_USB_SERIAL --report artifacts/fc3-hardware.json
./scripts/test-fc3-ascom.ps1 -Hardware -Serial YOUR_USB_SERIAL
```

The hardware probe records commands/replies; the Rust/Alpaca tests move 50
steps, return, halt during travel, change settings, and restore the original
position/settings. The COM test runs simultaneous 64-bit and 32-bit client
processes, asserts one server/worker, disconnects the first client, and moves
20 steps out/back with the second. Native NINA tests cover move completion and
cancellation, with optional `REGAIN_TEST_FC3_SERIAL` for hardware validation.
See [recorded evidence](focuscube3-evidence.json) and
[serial trace](focuscube3-serial.jsonl). The identity suffix is redacted in the
public trace. Firmware defaults were restored: position 1,250, speed 400,
backlash 0, normal direction.
