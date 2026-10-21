# Recover a camera with USB reset

Regain 0.5.0.0 and later can escalate failed camera captures to a USB reset.
NINA, native ASCOM, and Alpaca use the same Rust recovery supervisor. This is
disabled by default.

In NINA or native ASCOM camera setup, turn on **Automatic USB reset**. Set
**Recoverable failures before USB reset** to 1–20; it starts at **2** when enabling
a previously disabled profile, allowing an ordinary reconnect first. Failed
connections count across capture requests. The supervisor resets at most once
per outage and capture, after verifying the selected camera.

Before an exposure starts, **Pre-exposure reconnect attempts** bounds connection
and settings restoration (three attempts by default). An enabled USB reset can
help this preparation even for a long exposure. Once start has been dispatched,
the replacement count and exposure-duration limit decide whether another
exposure may be taken. A reset never grants permission to replace a long exposure.
Background telemetry holds its last readings during an outage; it does not
repeatedly open the camera.

Same-frame download retries happen before replacement. Reset abandons a retained
frame, then reconnects the same camera and restores camera/cooler settings. It
never turns a failed download into a successful image. Lost acknowledgements for
persistent settings stop automatic replay because a write may already have taken
effect.

## Supported cameras

USB recovery binds a camera's verified serial to its physical USB device before
imaging. It supports ASI585MM Pro, ASI662MC, ASI676MC, ASI2600MM Pro/Duo,
ASI6200MM Pro, and ASI220MM Mini USB identities, in either SDK or direct mode.
It is not general reset support for every camera supported by the ZWO SDK.
Unknown models, missing/ambiguous serials, and busy devices fail closed. Existing
camera selection and serial verification remain in force after reset.

## Windows: enable before imaging

Keep NINA unelevated. Enable Regain's USB recovery helper before starting an
unattended session; automatic recovery never launches a UAC prompt.

1. Disconnect the camera in the imaging application. Select its model and exact
   serial in camera setup.
2. In **Recovery**, read the note that cycling camera USB may disconnect
   peripherals connected through the camera's internal hub. Turn on **Automatic
   USB reset** and approve the Windows elevation prompt using the
   same Windows account. Setup reads the selected camera's identity and closes
   it; enabling the helper does not reset USB or leave the camera open. A denied
   prompt or failed preparation leaves the option off.
3. Confirm that the status shows the helper ready for that camera; **Check status**
   reads readiness without elevation or resetting USB. Use a threshold of **2**
   to try an ordinary reconnect first, then connect. Regain checks helper
   readiness before imaging. Enable each additional camera separately; NINA,
   native ASCOM camera slots and Windows Alpaca share the helper in the same
   Windows user session.
4. The elevated helper stays available until disabled, Windows sign-out or
   reboot. It runs separately from the camera worker and supervisor, and survives
   their restarts and NINA or ASCOM application restarts. It is not an installed
   service or an automatic startup task.
5. Turn the option off to disable resets and revoke approval for the selected
   camera. Other approved cameras remain enabled; the helper exits when the last
   approval is removed. Disable all approved cameras before updating Regain.

The toggle saves immediately, including when the setup dialog is later canceled.
Its saved setting and the helper's current authorization are separate: after
sign-out, reboot or a stopped helper, a saved enabled profile needs **Enable
helper** in setup before imaging. This always requires the camera to be
disconnected. Missing or unapproved helper access produces a setup error;
automatic recovery does not request elevation.
Disconnect before changing the toggle or threshold.

The helper accepts only approved camera bindings from the same Windows user
and session. Approval includes VID/PID, serial and physical attachment; each
reset also verifies the device's current arrival generation. It resolves the
camera's immediate parent hub and verifies the camera at that port before
issuing `IOCTL_USB_HUB_CYCLE_PORT`. It requests one downstream port, rather than
a whole hub or host-controller reset. **This does not guarantee physical
isolation:** a camera's internal hub or electronics can interrupt its other
USB devices. It is also not equivalent to unplugging the camera's entire USB
cable: USB 2 peripherals can use a separate hub branch that this camera-port
request does not cycle. The ZWO driver stays installed.

The privileged process does not load the vendor camera SDK or execute commands
supplied by clients. A local named pipe restricts access, requests and responses
are bounded, and a stalled reset has a hard process deadline. If preparation
is lost during imaging, recovery reports the failure without elevating again.
The production Windows port cycle does not use device disable/enable and does
not require a Windows restart.

For Windows Alpaca or a terminal, use the shared executable while the camera is
disconnected. Substitute its verified serial:

```powershell
$cameraUsbTarget = & .\regain-device.exe zwo camera-direct --usb-target 'ZWO ASI6200MM Pro' '<camera-serial>'
& .\regain-device.exe usb helper prepare $cameraUsbTarget
& .\regain-device.exe usb helper status $cameraUsbTarget
# Revoke only this camera's approval:
& .\regain-device.exe usb helper revoke $cameraUsbTarget
# Stop the shared helper and revoke every camera's approval:
& .\regain-device.exe usb helper stop
```

Preparation reads the selected camera's identity and closes that handle. The
helper does not keep a camera open, so normal camera access remains exclusive
to its frontend. For Alpaca, also set `recovery.usbResetAfterFailures` to the
desired positive threshold in the camera profile; helper preparation alone does
not enable the recovery policy. The CLI `stop` command affects every approved
camera in that Windows session.

## Camera-hub peripherals

A port cycle can disconnect a guide camera, filter wheel, focuser or other USB
device attached through the imaging camera. Keep USB reset disabled when that
disruption would be unsafe for the sequence, or connect those peripherals to
independent USB paths. Related power and companion-port behavior depends on the
hardware and cannot be inferred reliably from the camera model alone.

Regain verifies and restores the selected camera. A successful camera reconnect
does not establish that other peripherals reconnected. Recovery logs warn about
this before the reset and after camera identity is verified. The Windows helper
also logs USB instances observed missing, added or reenumerated around the cycle;
brief disconnects can escape that inventory. It does not reset an entire hub to
try to recover other devices.

### NINA peripheral reconnection

The retryable camera's **Recovery** tab has **Verify optical devices and recover
USB connections**, enabled by default, and **Peripheral disconnect window**,
15 seconds by default (1–60 seconds). Changes apply at the next camera connection.

With Regain's native NINA EFW and CAA providers selected, the camera reads fresh
hardware identity and optical state before capture, before any replacement
following a camera USB reset, and before delivering the image. These reads do
not trust NINA's cached `Connected` flag. A changed or missing filter slot,
rotator angle, device selection or hardware serial withholds the image and
blocks later captures until the camera is explicitly reconnected after checking
the equipment. No filter change, focus offset, rotation or guiding operation is
replayed. CAA angle comparisons allow 0.01 degrees of readout noise.

An exposure that overlaps an EFW/CAA disconnect or optical command is withheld even when the final
slot or angle matches: matching endpoints cannot prove the filter or rotator
stayed put during the exposure. Once the same idle optical state is verified,
the next fresh capture can proceed. A replacement following a camera reset gets
a new connection anchor only after the supervisor's pre-replacement verification.
Changed or unknown optics still require explicit reconnection and inspection.

The native EFW and CAA session code also handles spontaneous USB read failures,
independently of camera USB reset. It closes the stale worker and makes at most
three attempts, with two-second delays, to reopen the selected serial. Recovery
requires a previously verified idle state and the same state after reopening:
EFW slot and slot count; CAA mechanical angle, logical angle, logical offset and
travel limit and reverse setting. Reconnection restores the saved CAA logical sync; it does not
reset the mechanical origin or move the rotator. An active/unknown move, lost
write acknowledgement, changed optical state or exhausted attempts requires an
explicit disconnect/reconnect. A manual disconnect is never undone by polling.
A previously connected native optical device becoming disconnected is not silently
omitted from verification, even if NINA disconnects its driver after a polling
error. The camera must be explicitly reconnected to accept a changed optical
configuration. Manual disconnects are never automatically reversed.
The shared session code gives the same read recovery to native Windows ASCOM
EFW/CAA clients, preserving their shared connection leases during worker recovery.
This does not add spontaneous reconnection to third-party ASCOM drivers or to
the Rust Alpaca accessory workers. EAF motion and backlash are not replayed.
Recovery diagnostics appear in NINA's log and the accessory/rotator log.

The [acceptance evidence](optical-recovery-evidence.json) records the October 21
installed-NINA pass: ten capture checks with the physical ASI6200MM Pro, five
saved full-frame 5-second RAW16 images, and injected EFW/CAA worker failures.
Changed optics, interrupted exposures and a disconnected wheel returned no image;
the verified next capture succeeded without a move replay. The
[test procedure](../tests/fixtures/nina-optical-acceptance/README.md) is repeatable.
These fixtures do not establish physical EFW/CAA unplug or power-loss recovery.

A subsequent [physical CAA pass](physical-caa-recovery-evidence.json) used the
CAA-M54 and the same camera in installed NINA. Two exact downstream USB port
cycles invalidated the CAA handle and changed its Windows arrival generation;
the camera's generation stayed unchanged. Regain reopened the same CAA serial
on its first recovery attempt and verified the unchanged idle 90-degree position,
logical offset, reverse and limit. A cycle before capture allowed a fresh image.
A cycle during exposure withheld that frame; the next two captures succeeded
without manual reconnection. Five full-frame FITS images, including the baseline,
had distinct pixel hashes. No movement or origin change was requested.
The original installation and profile were restored. This verifies CAA USB
re-enumeration, not whole-cable power loss or an extended absence. Physical EFW
reconnection remains untested.

For other NINA providers, the camera's existing reconnection pass runs after
Regain initiates a camera USB reset. The disconnect window applies to that pass;
it is not a blanket reconnect trigger for unrelated or intentional disconnects.

Regain records which NINA devices were connected before the reset and watches for
their disconnect reports within that window. Once the camera returns and the
window closes, it reconnects the affected devices through NINA's own
disconnect, rescan and connect operations. Devices already disconnected before
the reset or reporting a disconnect later are left alone. A changed profile or
device selection prevents automatic reconnection. Attempts are bounded; a
timed-out NINA connection is never overlapped by another attempt.

Replacement exposures wait for this pass. For filter wheels, focusers, rotators
and flat panels, Regain compares their reported optical state before and after
reconnection. It does not move a wheel or focuser to restore that state: ordinary
filter changes can also apply focus offsets. A failed reconnection, unknown or
changed optical state, or timeout blocks further captures until the camera is
disconnected and reconnected after checking the equipment.

Connection monitoring covers NINA's filter wheel, focuser, rotator, guider,
switch, flat panel, weather, safety monitor, telescope and dome mediators. It can
only detect what those drivers report. In particular, a guide camera can drop
while PHD2's connection to NINA stays up; that is not proof that guiding recovered.
Regain does not restart guiding, move equipment, or restore switch outputs.
NINA's normal guiding, settling and safety checks still apply. Native ASCOM and
Alpaca cannot reconnect equipment owned by another application.

## Linux

The default uses the existing pure Rust `nusb` transport's USB device reset
(`USBDEVFS_RESET`). Allow access to the selected camera with the existing scoped
USB permissions. Regain does not detach kernel drivers or seize another owner's
interfaces. It checks both physical location and device address before resetting;
if the camera has already reenumerated, reconnect to establish a fresh binding.

The Alpaca **Linux: cycle the USB port** option instead disables the selected
downstream port for two seconds and re-enables it. It needs Linux 6.0+ port
`disable` controls and write permission for that specific port (or root).
For a service, grant only its service group the camera and selected port access.
No broad permission changes or automatic `sudo` invocation are performed.

Port cycling is not a guarantee that the hub removes 5 V. Hub switching hardware,
shared power groups, and USB 2/3 companion ports affect this. Neither operation
switches a camera's external 12 V supply. A full cold restart needs separately
controlled power hardware; external power-output integration is not implemented.

## Limits and diagnostics

The helper and rediscovery have bounded deadlines. Once a reset is dispatched,
abort waits for that operation to finish so a port cycle can restore the port;
no replacement exposure starts after cancellation. Abrupt host shutdown or power
loss can still interrupt a cycle. Logs identify binding, helper readiness,
reset, return, peripheral-disconnect risk and failure. Frame metadata includes
`usbResets` separately from replacement counts.

Profiles use `recovery.usbResetAfterFailures` (0 disables, 1–20 enables) and
`recovery.usbPortCycle` (false by default; Linux port-cycle selection).
`recovery.preExposureReconnectAttempts` (1–10, default 3) bounds preparation.
These settings do not increase the replacement-exposure budget. The NINA and
native ASCOM USB reset toggle writes `usbResetAfterFailures`: off saves 0;
on saves the selected positive threshold after helper preparation succeeds.

## Validation

Simulation tests kill an actual worker process and check recovery through both
SDK and direct Alpaca paths. Unit tests cover disabled defaults, thresholds,
one-reset limits, exposure limits, cancellation, and restoring settings.
Windows ASI6200MM Pro validation on October 12, 2026 exercised explicit helper
preparation, an unelevated manual port cycle, automatic USB recovery after
worker termination in both direct and SDK modes, and helper shutdown. Three
physical cycles completed in 208–210 milliseconds. Eight full-frame
9576 × 6388 RAW16 bin-1 images at five seconds covered baseline, replacement
and two fresh captures per backend; camera serial, changed USB arrival
generation and distinct image hashes were verified. Old arrival bindings were
rejected without another cycle. Physical readback confirmed cooler and dew
heater off at the end. Only the selected camera was observed reenumerating;
brief collateral disconnects may escape the inventory.

An installed NINA 3.2 acceptance pass on the same day used Regain's direct camera
plugin and ZWO's CAA ASCOM driver. The helper was prepared and stopped through
NINA's setup window. Two automatic camera cycles completed in 210 and 217 ms
without requesting elevation during capture. Seven full-frame five-second
RAW16 bin-1 FITS images covered the baseline, two replacement captures and two
fresh captures after each recovery; all pixel hashes differed.

NINA did not report a CAA disconnect during the first camera cycle. During the
second, a separate, exact-device Windows disable attempt caused ZWO's driver to report
the CAA disconnected. Regain reconnected the same ASCOM selection, waited for
NINA's fresh position poll, and verified its unchanged 90-degree optical state
before allowing the replacement exposure. No rotator movement was commanded.
The Windows disable fixture itself reported reboot required, so its intended
disable/hold/enable sequence did not pass. Cleanup cleared its pending disabled
configuration and confirmed a healthy, accessible CAA; Windows retained a reboot
required flag. That flag came from the separate test-only CAA PnP disable
fixture, not Regain's camera USB reset. This was an injected peripheral failure,
not collateral loss caused by cycling the camera's USB 3 function.

Those passes preceded the session-long helper and setup toggle. A subsequent
installed NINA pass verified the toggle and the same elevated helper process
surviving two NINA exits and a camera supervisor restart. One automatic physical
USB cycle completed in 215 ms. Nine full-frame five-second RAW16 bin-1 FITS
images had distinct pixel hashes, including two captures after the supervisor
restart. The CAA driver continued reporting connected at 90 degrees at its
two-second polls; Regain did not invoke a CAA reconnect. The replacement exposure
waited for the full peripheral observation window. Windows recorded a new camera
arrival but no new CAA arrival, and their enumerated USB 3 camera and USB 2 hub
paths were separate. This verifies no observed CAA disconnect, not uninterrupted
physical USB presence or recovery from a whole-cable cycle. Turning the toggle
off immediately saved the disabled policy, removed this camera's approval and stopped the last
helper, even though the setup dialog was then canceled. No Windows restart was
needed for camera recovery.

Windows ASI676 manual reset and a fresh RAW16 frame were also verified before
the prepared-helper change.
Linux compiles and is covered by simulation and port-control fixture tests;
physical Linux reset/power switching has not been validated.

Run `python scripts/test-usb-helper.py` for bounded, unelevated rejection checks
without preparation, UAC or a physical reset, and
`python scripts/test-usb-recovery.py` for simulations. The opt-in hardware
test requires an idle, authorized camera's `--hardware-serial SERIAL` and
`--hardware-model 'ZWO ASI6200MM Pro'` (ASI676MC is the default model). It takes
a full-frame baseline, intentionally kills its worker during a full-frame
exposure, resets USB, and captures a replacement followed by two fresh full
frames in both direct and SDK modes. Use `--evidence PATH.json` to retain
diagnostics and frame hashes. On Windows, prepare its helper beforehand; the
test checks existing approval and never asks for elevation.

References: [Windows hub port cycle](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/usbioctl/ni-usbioctl-ioctl_usb_hub_cycle_port),
[Linux port power controls](https://docs.kernel.org/driver-api/usb/power-management.html),
[hub switching limitations](https://github.com/mvp/uhubctl#what-is-usb-per-port-power-switching).
