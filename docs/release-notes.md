ZWOgain (ZWO Again) provides a process-isolated ZWO camera driver for NINA 3.2.0.9001+.
This first release includes the NINA plugin and a standalone Windows camera
exercise kit for contributing new camera models.

- Runs the ASI SDK in a supervised Rust process and restores camera settings
  after recoverable failures.
- Retries exposures of 30 seconds or less by default, with a configurable
  cutoff, retry count, reconnect delay and cooling recovery limits.
- Includes an embedded camera/recovery logo for NINA's Plugin Manager.
- Organizes setup into Camera, Recovery, Cooling and Advanced tabs, with clear
  ASI2600MM Pro and ASI220MM Mini guide choices.
- Waits for measured temperature and cooler output to recover before retrying,
  with a temporary NINA readiness-timeout extension for longer recovery.
- Allows ASI2600MM Pro direct exposures up to 2,000 seconds. The default
  automatic retry cutoff remains 30 seconds.
- Tries ready-frame SDK downloads twice by default regardless of exposure
  duration. The exposure cutoff governs full recapture, including reconnect
  and cooling restoration, rather than rereads.
- Allows retained-frame transfer retries at any supported exposure length on
  the direct ASI2600/ASI676 paths. Replacement exposures still obey the cutoff.
  ASI2600 tests recovered the same pixels after canceled USB reads and a real
  read deadline; cable removal and power loss remain unverified.
- Adds experimental SDK-free ASI2600MM Pro main and ASI220MM Mini guide capture,
  factory correction, software binning, main cooling/dew control, and persisted
  opt-in SDK fallback with serial verification and the shared retry policy.

Validated with ASI676MC and both ASI2600MM Pro Duo sensors, simulator fault injection,
and interactive NINA captures. Duo cooling and SDK fallback after worker termination
were hardware-tested. ASI6200/P25 and natural USB transfer failures need separate validation. Public same-frame re-download is
conditional on SDK ready status, with two read retries by default.

**NINA plugin:** download `ZwoGain-0.1.0.0.zip`, or add
`https://nina-plugins.psf-guard.com/` as a NINA plugin source and install ZWOgain.
For manual installation, extract the plugin ZIP into
`%LOCALAPPDATA%\NINA\Plugins\3.0.0\ZwoGain` while NINA is closed.
The ZWO Windows camera driver must already be installed. Select **ZWOgain
Retryable Camera** and choose your camera in its setup dialog. The SDK backend
is the default; the direct backend and SDK fallback are opt-in.

**Camera exercise kit:** download `ZwoGain-CameraKit-0.1.0.0-win-x64.zip`, extract
the entire folder, and run `ZwoGain-CameraKit.exe` with other camera applications
disconnected. Requires Windows x64 and the ZWO driver; no Python, Rust or NINA
installation. The picker runs quick or extended RAW16 exercises and produces
a local ZIP of settings, SDK/USB transactions, calibration and optional pixel
samples. Nothing uploads automatically. Review the bundle before sharing.
See the [kit instructions](https://github.com/theatrus/zwogain/blob/v0.1.0.0/scripts/camera-kit/README.md)
and [hardware results](https://github.com/theatrus/zwogain/blob/v0.1.0.0/docs/camera-kit-validation.md).

Apache-2.0; bundled third-party components retain their own licenses.
Release plugin DLLs, Rust workers and the kit executable are signed by
StackFoundry LLC. Checksums cover the final signed packages. Local and ordinary
CI builds are unsigned. ZWOgain is independent and is not affiliated with ZWO.
