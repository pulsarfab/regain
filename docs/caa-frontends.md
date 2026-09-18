# CAA in NINA and ASCOM

ZWOgain controls the CAA through its Rust USB HID worker, without the ZWO SDK.
There is one **ZWOgain CAA Rotator** entry in NINA and one in the Windows ASCOM
Chooser. Select the physical device in setup; its serial is saved. A device
that is missing or busy is not replaced with another CAA automatically.

These frontends are new in the source tree. They are not in release v0.2.0.0.
ZWOgain is independent and is not affiliated with or supported by ZWO.

## Setup

In NINA, update the plugin, restart NINA, open Equipment → Rotator and select
**ZWOgain CAA Rotator**. Open its setup gear. In ASCOM, use the same-named
Chooser entry, or **ZWOgain ASCOM → CAA rotator setup** in the Start menu.

1. On **Device**, refresh the list, choose the CAA, and save the selection.
2. Connect in the main application. To test within setup, use its connection
   button. A connection opened only for setup is closed when the dialog closes.
3. Use **Motion** for ordinary moves and Halt. **Settings** controls beep,
   reverse and the device alias.

The CAA uses Windows' HID driver; it does not need the ZWO camera driver.
Only one controller may hold it at a time. The NINA and ASCOM frontends cannot
connect to the same CAA simultaneously. The ASCOM rotator is local USB through
the Rust worker; the Alpaca server currently serves cameras only.

## Origin and travel limit

**Set current position to mechanical 0°** sends the device's origin-reset
command. It does not rotate the motor. **Set mechanical reference** can assign
another value between 0 and 360. Both change where the firmware's travel
interval begins. They require an idle, fault-free device.

**Sync sky angle** only changes the logical coordinate. It never resets the
mechanical origin. Reverse changes logical direction; mechanical moves retain
their physical direction. The worker preserves the current logical angle
when changing reverse or the mechanical reference.

The normal limit is 360°. Limits of 1–361° are supported; 361° is experimental
and was tested on CAA-M54 firmware 1.1.1. A limit cannot exclude the current
position. Larger firmware limits have not been validated. Ordinary moves are
checked before sending USB commands: an out-of-range target can otherwise
cause backward motion rather than a firmware error.

ASCOM's standard position properties are normalized to 0–360°. Use the Status
action to see the raw mechanical angle, including 360 or 361. Standard ASCOM
absolute commands use 0 ≤ angle < 360; setup exposes the extended endpoint.

## Explicit multi-turn travel

The **Multi-turn** tab accepts physical travel between -450 and +450 degrees.
It requires the clearance/cable-slack checkbox for each move. It divides the
move into segments of at most 90°, resetting the mechanical reference for
each segment. Negative travel uses an assigned positive reference and moves
toward zero. Reverse does not change the sign of this explicit physical travel.

Five 90° forward moves with intervening resets completed 450° of reported
travel on the attached CAA. Five reverse moves returned it. This bypasses
cumulative cable-wrap protection; it is not enabled for normal NINA/ASCOM moves.
The last segment's mechanical reference remains in effect afterward. Sky
coordinates are compensated, but the frontend marks the plate-solve sync stale
after an explicit reference or multi-turn operation.

Halt discards queued segments before stopping. A bad read, fault, unexpected
direction, unexpected position, or expired deadline stops the operation.
It never retries a movement or starts an automatic unwind after failure.
The worker gives ordinary moves 120 seconds and each multi-turn segment
30 seconds. These are polling deadlines; OS HID calls are synchronous.

## ASCOM actions

The COM driver implements `IRotatorV3`: Connect through `Connected`, Move,
MoveAbsolute, MoveMechanical, Sync, Reverse, Halt, and position/busy properties.
Moves return after submission; poll `IsMoving` for completion. NINA's native
move methods await completion and halt on cancellation.

Additional controls are exposed through `SupportedActions` and `Action` in
both frontends. Action names are case-insensitive. Parameters are JSON, except
the read actions and ResetOrigin, which accept an empty string. Results are JSON.

| Action | Parameters | Effect |
| --- | --- | --- |
| `ZwoGain.CAA.Status` | empty | Raw/logical position, target, limit, busy state and faults |
| `ZwoGain.CAA.Settings` | empty | Beep and reverse |
| `ZwoGain.CAA.Identity` | empty | Model, firmware, serial and alias |
| `ZwoGain.CAA.ResetOrigin` | empty | Set the current physical position to mechanical zero |
| `ZwoGain.CAA.SetReference` | `{"degrees":152}` | Assign a mechanical reference without movement |
| `ZwoGain.CAA.SetLimit` | `{"degrees":360}` | Set the firmware travel limit |
| `ZwoGain.CAA.SetBeep` | `{"enabled":true}` | Change beep setting |
| `ZwoGain.CAA.SetAlias` | `{"text":"Rotator"}` | Save up to eight printable ASCII characters |
| `ZwoGain.CAA.RotateUnwrapped` | `{"degrees":450}` | Start explicit segmented physical travel |

For example, `driver.Action("ZwoGain.CAA.ResetOrigin", "")` resets mechanical
zero. It is different from `driver.Sync(0)`, which only changes sky coordinates.
Raw CommandString/CommandBlind/CommandBool access is not exposed.

## Persistence and failures

NINA and ASCOM save their selected serial and logical offset separately under
`%LOCALAPPDATA%\ZwoGain\Rotators\nina.json` and `ascom.json`. Device settings
are read on connection; connecting does not reset the origin or change limits.
After an unfinished reference operation, a new connection drops the saved sky
offset and requires a new sync. Reference changes made in another application
also require a new sky sync.

The worker owns the exclusive HID handle. Disconnect and end-of-input halt
motion and discard queued segments. A timeout or process failure is reported;
movement commands are never replayed. If the worker itself crashes, firmware
may finish the current segment. Do not treat process termination as a confirmed
motor stop. No automatic USB reset or mechanical-origin reset is used to recover.

Diagnostics go to NINA's log and
`%LOCALAPPDATA%\ZwoGain\Rotators\rotator.log`. Halt acknowledges a stored motion
error; a new move still requires healthy device status.

USB-handle reopen retains the reference. Physical power-loss retention,
internal flash/EEPROM storage, and encoder presence remain unverified. See
[protocol and hardware results](caa.md) and [sanitized traces](caa-reference-evidence.json).

## Build and package

`scripts/build.ps1` includes `zwogain-caa.exe`, the shared rotator frontend,
and protocol documentation in the NINA ZIP. `scripts/build-ascom.ps1` and
`scripts/build-ascom-installer.ps1` include them in the ASCOM package/installer.
The installer registers exactly one rotator for 32-bit and 64-bit clients;
the four existing camera entries are separate. Release signing covers both
frontend assemblies and the CAA worker.

## Hardware checks

On CAA-M54 firmware 1.1.1, NINA's equipment pane connected, moved from 152° to
153°, and returned. Setup reset mechanical zero while preserving the sky angle,
then restored the reference. Device selection survived a NINA restart.
Both 32-bit and 64-bit COM clients passed connection, sync, origin reset,
reference restoration, motion and 361° limit checks. The production worker
completed +450° and -450° segmented travel and restored the 152° reference.
These checks use device position reports, without an independent angle sensor.

To repeat the COM checks with one available CAA, run
`scripts/test-caa-ascom.ps1 -Hardware`. For segmented travel, with clearance
and cable slack for 450° forward and return:

```powershell
python scripts/inspection/validate_caa_worker.py --travel 450 --output artifacts/caa-worker-new.jsonl
```

The worker script stops on failure and does not automatically unwind an
uncertain move. Close other controllers before running either hardware test.
