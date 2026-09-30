# Recover a camera with USB reset

Current source/CI builds can escalate failed camera captures to a USB reset.
NINA, native ASCOM, and Alpaca use the same Rust recovery supervisor. This is
disabled by default.

In camera setup, set **USB reset after failed attempts** to **2** to try an
ordinary reconnect first. The count refers to failed capture attempts, after
their download retries. The supervisor resets at most once per capture and only
when another replacement exposure remains within the retry count and exposure
duration limit. Long integrations can still retry a retained download, but are
not reset or replaced unless their duration is allowed by that policy.

The sequence is: retry the download, reconnect and retry the exposure, reset USB
if the configured failure threshold is reached, reconnect the same camera,
restore camera/cooler settings, and start a replacement exposure. Reset abandons
the retained frame. It never turns a failed download into a successful image.

## Supported cameras

USB recovery binds a camera's verified serial to its physical USB device before
imaging. It supports the currently implemented ASI676MC, ASI2600MM Pro/Duo,
ASI6200MM Pro, and ASI220MM Mini USB identities, in either SDK or direct mode.
It is not general reset support for every camera supported by the ZWO SDK.
Unknown models, missing/ambiguous serials, and busy devices fail closed. Existing
camera selection and serial verification remain in force after reset.

## Windows

The shared `regain-device.exe` launches a separate elevated instance when needed.
It resolves the camera's immediate parent hub and checks the camera VID/PID and
connection status at that port before issuing `IOCTL_USB_HUB_CYCLE_PORT`.
It never resets the entire hub or host controller. The ZWO driver stays installed.

An interactive session may display a UAC prompt. Approve it within 60 seconds;
late authorization expires without resetting the camera. A denied or timed-out
request fails the capture with a diagnostic. For unattended operation, start the
host with administrator rights; an unelevated host cannot silently approve UAC.
The feature does not install a privileged service or change system permissions.

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
loss can still interrupt a cycle. Logs identify binding, reset, return, and
failure. Frame metadata includes `usbResets` separately from replacement counts.

Profiles use `recovery.usbResetAfterFailures` (0 disables, 1–20 enables) and
`recovery.usbPortCycle` (false by default; Linux port-cycle selection).
Raising the failure threshold does not increase the replacement-exposure budget.

## Validation

Simulation tests kill an actual worker process and check recovery through both
SDK and direct Alpaca paths. Unit tests cover disabled defaults, thresholds,
one-reset limits, exposure limits, cancellation, and restoring settings.
Windows ASI676 manual reset and a fresh RAW16 frame were verified on hardware.
Linux compiles and is covered by simulation and port-control fixture tests;
physical Linux reset/power switching has not been validated.

Run `python scripts/test-usb-recovery.py` for simulations. The opt-in hardware
test requires `--hardware-serial SERIAL` for an idle ASI676 and intentionally kills
its worker during a short exposure, resets USB, and captures a replacement frame.

References: [Windows hub port cycle](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/usbioctl/ni-usbioctl-ioctl_usb_hub_cycle_port),
[Linux port power controls](https://docs.kernel.org/driver-api/usb/power-management.html),
[hub switching limitations](https://github.com/mvp/uhubctl#what-is-usb-per-port-power-switching).
