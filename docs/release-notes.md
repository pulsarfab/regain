ZWOgain (ZWO Again) provides a process-isolated ZWO camera driver for NINA 3.2.0.9001+.

- Runs the ASI SDK in a supervised Rust process and restores camera settings
  after recoverable failures.
- Retries exposures of 30 seconds or less by default, with a configurable
  cutoff, retry count, reconnect delay and cooling recovery limits.
- Includes an embedded camera/recovery logo for NINA's Plugin Manager.

Validated with the ASI676MC, simulator fault injection, and interactive NINA
capture/recovery tests. ASI2600/6200 cooling recovery and natural USB transfer
failures still need hardware validation. Public same-frame re-download is
experimental and disabled by default.

Install through https://nina-plugins.psf-guard.com/ after registry publication,
or extract the ZIP into `%LOCALAPPDATA%\NINA\Plugins\3.0.0\ZwoGain` while NINA
is closed. The ZWO Windows camera driver must already be installed.

Apache-2.0; bundled third-party components retain their own licenses.
This initial workflow produces unsigned plugin binaries.
