ZWOgain (ZWO Again) provides a process-isolated ZWO camera driver for NINA 3.2.0.9001+.

- Runs the ASI SDK in a supervised Rust process and restores camera settings
  after recoverable failures.
- Retries exposures of 30 seconds or less by default, with a configurable
  cutoff, retry count, reconnect delay and cooling recovery limits.
- Includes an embedded camera/recovery logo for NINA's Plugin Manager.
- Organizes setup into Camera, Recovery, Cooling and Advanced tabs, with clear
  ASI2600MM Pro Duo main and ASI220MM Mini guide choices.
- Waits for measured temperature and cooler output to recover before retrying,
  with a temporary NINA readiness-timeout extension for longer recovery.
- Adds experimental SDK-free ASI2600MM Duo main and ASI220MM Mini guide capture,
  factory correction, software binning, main cooling/dew control, and persisted
  opt-in SDK fallback with serial verification and the shared retry policy.

Validated with ASI676MC and both ASI2600MM Pro Duo sensors, simulator fault injection,
and interactive NINA captures. Duo cooling and SDK fallback after worker termination
were hardware-tested. ASI6200/P25 and natural USB transfer failures need separate validation. Public same-frame re-download is
experimental and disabled by default.

Install through https://nina-plugins.psf-guard.com/ after registry publication,
or extract the ZIP into `%LOCALAPPDATA%\NINA\Plugins\3.0.0\ZwoGain` while NINA
is closed. The ZWO Windows camera driver must already be installed.

Apache-2.0; bundled third-party components retain their own licenses.
This initial workflow produces unsigned plugin binaries.
