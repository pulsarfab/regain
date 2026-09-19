ZWOgain 0.3.1.0 adds native EFW/EAF drivers and calibration, expands Alpaca,
and replaces the Windows camera ASCOM bridge with a native local driver.

- **EFW filter wheel:** SDK-free Rust USB support, native NINA and 32/64-bit
  ASCOM drivers, filter names and focus offsets, unidirectional movement,
  and explicit wheel calibration with live progress. Calibration preserves
  filter metadata, finishes at slot 1, and rejects duplicate motion commands.
- **EAF focuser:** native absolute positioning, halt, temperature, beep,
  reverse, hardware backlash, and travel-limit settings. Use zero hardware
  backlash when the imaging application supplies backlash compensation.
- **Native Windows ASCOM cameras:** four camera entries now use private local
  Rust workers and CAA-style setup dialogs, with shared recovery controls.
  Local ASCOM connections no longer require an Alpaca server. When upgrading
  from the bridge, select each camera once in the new setup dialog.
- **Alpaca:** the standalone server now exposes the CAA rotator, EFW filter
  wheel, and EAF focuser alongside cameras, with browser setup, serial-based
  selection, and shared client connections. EFW calibration is also available
  through the `ZwoGain.Calibrate` action in ASCOM, NINA, and Alpaca.
- **Windows connection fix:** accessory and camera workers accept the UTF-8
  preamble emitted by .NET Framework hosts. Regression tests exercise this
  behavior in both 32-bit and 64-bit COM clients.
- **Documentation:** Windows and Alpaca setup instructions, USB tracing
  evidence, and a README screenshot gallery. Screenshots use the production
  UI with simulated devices; physical hardware validation is documented separately.

The attached seven-position EFW-S-0 (firmware 3.6.2) and unmounted EAFN
(3.8.1) passed native Rust, Alpaca, and 32/64-bit ASCOM motion tests with
starting positions and settings restored. EFW calibration passed through the
SDK and native frontends in about 49 seconds. NINA provider integration and
setup-button tests passed with simulation. Interactive NINA autofocus,
ConformU certification, and Linux/macOS accessory hardware testing remain
outstanding. Earlier EAF firmware, EAF Pro/Bluetooth, and dual-disc EFWs are
not supported. Only one frontend may own a physical USB device at a time.

**NINA:** update ZWOgain from `https://nina-plugins.psf-guard.com/` and restart
NINA. For manual installation, close NINA and extract `ZwoGain-0.3.1.0.zip`
into `%LOCALAPPDATA%\NINA\Plugins\3.0.0\ZwoGain`. Requires Windows x64 and
NINA 3.2.0.9001 or later.

**Windows ASCOM:** run `ZwoGain-ASCOM-0.3.1.0-win-x64-setup.exe`. The installer
registers camera, CAA, EFW, and EAF drivers for both 32-bit and 64-bit clients.
Cameras need the ZWO Windows camera driver; the accessories use Windows HID.
The release also includes a portable ASCOM ZIP and
`ZwoGain-CameraKit-0.3.1.0-win-x64.zip` for camera data collection.

Binaries are signed by StackFoundry LLC. ZWOgain is Apache-2.0 and is not
affiliated with or supported by ZWO. Bundled software retains its own licenses.
