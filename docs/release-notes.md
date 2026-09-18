ZWOgain 0.3.0.0 adds CAA rotator support, an ASCOM installer, and more direct USB cameras.

- **CAA rotator:** SDK-free USB control in NINA and ASCOM, with motion, sync,
  mechanical zero reset, reference restoration, travel limits, beep, and alias.
  The tabbed setup saves device selection automatically and fixes first-run
  connection and display-scaling problems. One ASCOM rotator entry is installed.
- **ASCOM cameras:** four Windows camera slots, installed for both 32-bit and
  64-bit clients. A standalone Rust Alpaca server supports one saved slot per camera.
  Camera recovery is shared with NINA.
- **More direct cameras:** ASI2600MM Pro P25 and original ASI6200MM Pro, including
  cooling, defect correction, bins 1–4, and supported auxiliary controls.
- **Recovery:** NINA logs retries, cooler recovery, and SDK fallback. Direct USB
  diagnostics report failed chunks and bytes received. ASI2600 P25 can reopen
  its USB handle and verify a retained frame before returning it.
- **Portable Rust:** Linux and macOS builds support native USB access and dynamic
  SDK loading. Their standalone packages are available from CI; hardware testing
  on those platforms is still needed.

Direct camera support now covers **ASI676MC**, **ASI2600MM Pro** (original and
P25), **ASI6200MM Pro** (original and P25), and **ASI220MM Mini guide camera**.
The SDK remains the default. Direct USB capture and SDK fallback are optional.
Replacement exposures are limited to **30 seconds** by default; this is
configurable. Same-frame rereads have a separate retry limit.

Hardware checks used dark frames. Full 1,200-second captures passed in NINA on
both ASI2600 and ASI6200 editions. Cooler recovery and auxiliary controls were
checked on the newly supported cameras. The CAA-M54 passed motion, zero reset,
reference restoration, and installed 32-bit/64-bit ASCOM tests. Its replacement
dialog was checked inside NINA at 200% display scaling.

The 2,000-second camera limit remains untested. USB lifecycle tests used
controlled faults and Windows port cycling, not physical cable removal or
power loss. CAA multi-turn travel uses reference resets and requires enough
cable slack; ordinary moves keep the firmware travel limit.

**Install or update in NINA:** use `https://nina-plugins.psf-guard.com/` as a
plugin source, update ZWOgain, and restart NINA. Manual installs use
`ZwoGain-0.3.0.0.zip` in `%LOCALAPPDATA%\NINA\Plugins\3.0.0\ZwoGain` with NINA closed.
Requires Windows x64, NINA 3.2.0.9001 or later, and the ZWO Windows camera driver
for cameras.

**ASCOM:** run `ZwoGain-ASCOM-0.3.0.0-win-x64-setup.exe`. The release also includes
a portable ASCOM ZIP and `ZwoGain-CameraKit-0.3.0.0-win-x64.zip` for collecting
new camera data without Python or a compiler.

Binaries are signed by StackFoundry LLC. ZWOgain is Apache-2.0 and is not
affiliated with or supported by ZWO. Bundled software retains its own licenses.
