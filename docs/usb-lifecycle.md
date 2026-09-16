# USB lifecycle and retained frames

The direct driver can restart a download from camera DDR without exposing again.
On Windows, ASI2600MM Pro P25 recovery can also close and reopen the USB handle.
It stays within the configured read retry count and does not take a new exposure.
Recovery across process replacement remains a research command.

## Production changes

- ASI2600/6200 capture logs distinguish initialization, exposure, sensor readout,
  retained memory, download, and validation. The server logs image-ready or failure.
- USB failures include a category, chunk number, completed byte count, expected
  frame size, deadline flag, and available Windows/NT/USB or native status.
  Terminal error replies include an optional `transportFailure` object. Existing
  error text and protocol version remain compatible.
- `transferTimeoutSeconds` bounds each complete USB read attempt. NINA and the
  Rust supervisor use the configured download timeout, default 60 seconds.
  Every retained reread receives a fresh budget; exposure time does not consume it.
  Individual bulk requests remain capped at five seconds (some models allow a
  longer first request). Cancellation must still drain before buffers are freed.
  A control request already in progress or cancellation drain can extend the
  observed failure time beyond the transfer deadline. The outer watchdog remains.
- No partial or late frame is returned as a successful image.

## Automatic handle recovery

After two failed read attempts, the Windows ASI2600 P25 backend can close its
handle, wait three seconds, enumerate the original interface again, and verify
the serial saved before exposure. This requires at least one completed pixel
chunk from that exposure. Without that evidence, it keeps using ordinary sender
retries. Zero or one configured read retry cannot reach the handle-reopen step.

The driver saves SHA-256 hashes of completed chunks and checks them on replay,
excluding only the changing frame envelope. A mismatch fails the capture.
It preserves calibration and geometry in the same worker and restores cooler
output, setpoint, dew, fan and LED state without sensor initialization. The
frontend deadline includes time for reopening. Logs report the step; metadata
includes `handleReopens`, `retainedPixelsVerified` and `verifiedRetainedBytes`.

[Production-path evidence](usb-handle-recovery-evidence.json) covers real Windows
read cancellations near the beginning, middle and end of full frames, a 60-second
exposure, missing prefix evidence, and exhausted retry budgets. Traces show one
exposure start and no sensor writes between reopen and reread. A second complete
replay matches every interior pixel. These are cancellations, not physical faults.

[Cooler evidence](usb-handle-environment-evidence.json) covers the same path with
active cooling: temperature changed from 30.4 to 29.4 C while the 26 C target and
11% output were preserved. Dew, fan and LED settings survived, and a subsequent
capture used a new offset. This restores state during the same frame's download;
the existing supervisor still handles cooling waits before replacement exposures.

The [supervisor test](usb-handle-supervisor-evidence.json) uses the same private
pipe adapter as NINA. It recovers a 60-second frame with one exposure and forwards
the recovery logs. A 31-second frame with an exhausted read budget fails without
recapture or SDK fallback, even when fallback is enabled.

The CLI accepts `--transfer-timeout-seconds`. Its watchdog now accounts for the
configured transfer/replay budgets instead of assuming every capture needs only
15 seconds beyond the exposure.

## Reopen experiments

With a capped ASI2600MM Pro P25 on Windows USB3, the test sequence is:

1. Take a full 6248 × 4176 RAW16 exposure and download a prefix.
2. Close the exclusive USB handle; wait 0–5 seconds.
3. Open the same enumerated interface and verify its serial without publishing it.
4. Stop/reset/restart the retained sender without initializing the sensor.
5. Download the frame from byte zero and compare the prefix.
6. Replay the full frame again and compare all interior pixel bytes.

The matrix covers a 1 KiB prefix, 12 MiB, and the final full MiB chunk, with
0.1-, 2-, and 60-second exposures. USB traces must show exactly one exposure-start
command and no sensor writes between reopening and the next bulk read.
The first restart can time out; another restart can then succeed. Increasing
the reconnect delay alone does not establish a fix for this behavior.

The replacement-process experiment first records a complete raw interior hash
and deliberately leaves DDR intact. A fresh worker retrieves that frame using
only the retained-sender sequence. Another test terminates a reader after its
12th successful 1 MiB completion, then verifies the full hash in a fresh worker.
This tests abrupt process exit during a replay of a known frame. It does not
prove recovery from an undrainable kernel request, USB removal, or power loss.

Frame envelopes contain changing counters. Comparisons exclude only the first
and last four-byte envelopes; a counter alone is not a reliable cross-process
frame identity. Pixels are compared before defect correction or software binning.

## Run the tests

Disconnect other camera apps and leave the camera capped. On Windows, use the
inspection Python environment with Frida:

```powershell
cargo build -p zwogain-direct --release --locked
.reference/inspection-venv/Scripts/python.exe scripts/inspection/validate_usb_lifecycle.py --output artifacts/usb-lifecycle-new
.reference/inspection-venv/Scripts/python.exe scripts/inspection/validate_handle_recovery.py --output artifacts/handle-recovery-new
.reference/inspection-venv/Scripts/python.exe scripts/inspection/validate_handle_environment.py --output artifacts/handle-environment-new
.reference/inspection-venv/Scripts/python.exe scripts/inspection/validate_handle_supervisor.py --output artifacts/handle-supervisor-new
.reference/inspection-venv/Scripts/python.exe scripts/inspection/validate_port_lifecycle.py --output artifacts/port-lifecycle-new
```

The output directory must be new. `results.json` contains sanitized evidence;
`private-*.jsonl` files contain local transport traces and must be reviewed before
sharing. The script rejects changed binaries, additional exposures, mismatched
prefixes, and mismatched full-frame hashes.

The recorded [hardware evidence](usb-lifecycle-evidence.json) includes all five
reopen cases and both process-replacement cases. A separate
`validate_transfer_deadline.py --output artifacts/NEW-deadline.json` test sets a
5 ms whole-read budget on the real camera, checks that `status` and `download`
return the same structured failure with no pixels, then requests and validates
a fresh full frame with the normal timeout.

Individual research commands:

```powershell
target/release/zwogain-direct.exe --capture-2600-p25 --gain 100 --offset 50 --reopen-after-bytes 12582912 --reopen-delay-ms 1000 --replay
target/release/zwogain-direct.exe --capture-2600-p25 --keep-retained
target/release/zwogain-direct.exe --verify-retained-2600-p25 --expected-wire-sha256 HASH
```

Use `wireInteriorSha256` from the preceding capture for `HASH`. For a cropped or
binned capture, supply its physical `rawWidth`/`rawHeight` to the verifier. It
returns verification metadata only, never an image to a frontend. Ordinary
captures still clear DDR during cleanup; `--keep-retained` is a CLI-only opt-in.

## Before production worker replacement

The supervisor needs a retained-frame record containing camera identity, exposure
generation, physical geometry, correction context, and evidence from completed
chunks. It also needs to know whether any initialization, new exposure, USB reset,
or other owner invalidated that record. If continuity cannot be established, it
must reject the retained image and apply the existing replacement-exposure limit.
The full hash used by this experiment is unavailable when the original download
never finished. In-process chunk hashes now provide partial evidence, but passing
that evidence and the correction context safely across worker death is still open.

## Windows device restart

The installed Cypress-based driver offers two camera-scoped operations that
worked without elevation on this system:

```powershell
target/release/zwogain-direct.exe --reset-port-2600-p25
target/release/zwogain-direct.exe --cycle-port-2600-p25
```

These CLI-only experiments require exactly one ASI2600 P25 on USB3, driver
`0x01020200`, no other camera owner, and cooling disabled. They use the documented
Cypress `RESET_PARENT_PORT` (`0x220030`) and `CYCLE_PORT` (`0x220028`) operations,
then close/reopen the handle and verify the original camera identity. Both have
a 30-second outer watchdog. They are unavailable through the server protocol.

The [hardware test](usb-port-lifecycle-evidence.json) found that both operations
leave the retained-status register at `0x15`, but all three frame rereads time out
before the first chunk. A fresh full-frame exposure works after each. Cycling
changes Windows' device arrival time; the other attached camera's arrival and
status remain unchanged. This does not prove that externally powered DDR was
erased, only that our retained sender cannot read it after the reset. Neither
operation is part of automatic same-frame recovery.

Sources: [Cypress driver reference, sections 6.3 and 6.15](https://community.infineon.com/gfawx74859/attachments/gfawx74859/usb-superspeed-peripherals/35585/1/CyUSB.pdf)
and the [Cypress IOCTL header](https://github.com/wuxx/nanoDLA/blob/a30f61bddf5f5d62ee6d97231939aa2e83a85c5a/doc/CypressTools/Drivers/CyUsb/inc/cyioctl.h).

Windows also offers a separate PnP device-node restart:

`scripts/inspection/restart-camera.ps1` calls Windows `pnputil /restart-device`
for exactly one ZWO imaging device instance. It requires an elevated PowerShell,
rejects wildcard/hub targets, checks that the device returns, and writes a
sanitized result. It never requests a PC reboot. Example from an administrator
PowerShell, after disconnecting camera apps:

```powershell
$camera = @(Get-PnpDevice -PresentOnly | Where-Object InstanceId -like 'USB\VID_03C3&PID_260E\*')
if ($camera.Count -ne 1) { throw 'Select one camera instance explicitly.' }
./scripts/inspection/restart-camera.ps1 -InstanceId $camera[0].InstanceId -OutputPath artifacts/pnp-restart-new.json
```

This is a device-node restart, not proof of a USB power cycle. See
[Microsoft's command reference](https://learn.microsoft.com/en-us/windows-hardware/drivers/devtest/pnputil-command-syntax).
On this test machine, the unelevated attempt returned access denied and the
elevation request was canceled. No PnP restart has been validated yet. The plugin
does not automatically request administrator access or run this script.

Still open: a definitive sensor-readout-complete flag, physical USB reattachment,
endpoint stalls, short reads on real hardware, recovering DDR reads after USB reset,
Linux/macOS hardware tests, and an overlapped read queue. Power loss must be treated
as loss of DDR. No arbitrary byte-offset resume command has been found.
