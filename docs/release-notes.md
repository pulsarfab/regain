# PulsarFab regain 0.5.4.0

This patch fixes Direct USB acquisition over USB 2 high-speed connections for
supported ZWO cameras, plus two ASI2600MM Duo retained-frame acquisition bugs.
SDK remains the default; Direct USB remains experimental and opt-in.

- Accept validated USB 2 high-speed endpoint layouts as well as USB 3 for the
  existing Direct USB models. Unsupported or ambiguous descriptors fail before
  camera programming. This does not add support for every camera in the ZWO SDK.
- Pace short ASI662MC/ASI676MC USB 2 frames without changing their integration
  time, fixing the observed ASI676 retained-replay mismatch.
- Wait for the complete programmed Duo sensor frame interval before freezing
  DDR. The old fixed wait could leave stale lower rows despite identical replay.
- Pad tiny Duo physical reads to at least 128 KiB on both USB 2 and USB 3, then
  apply factory correction and crop to the exact requested ROI. This fixes the
  observed zero-byte timeout at 64x64 / 100 ms.
- Preserve explicit camera selection, cached identity, bounded deadlines and
  same-frame retained retries. No new discovery sweep or automatic port reset
  is introduced by these changes.

Operator-authorized Windows testing covered ASI662MC, ASI676MC, ASI2600MM Pro
P25, ASI2600MM Duo and both ASI6200MM Pro revisions over USB 2. The 2600 P25
and Duo each passed 40 cases / 42 frames; Duo also passed the USB 3 regression.
Checks included ROI/binning, full-row offset transitions, retained replay,
interrupted reads, timeouts and worker recovery. Full-frame exposures were
tested through 30 seconds and selected ROI exposures through 60 seconds.
Evidence is tied to the exact builds recorded in the coverage report.

**Limits:** ASI6200 extreme-gain frames exceeded the conservative individual-row
uniformity check despite valid transfers and matching replay. Noise versus
freshness at these settings remains uncharacterized; these are qualified
results, not unconditional matrix passes. Physical disconnect/port-reset
recovery, cold power-up, other USB controllers, Linux/macOS hardware and optical
accuracy remain unvalidated. No advertised 2,000-second maximum was validated
in this pass. No private images, camera serials or calibration payloads are bundled.

See the [USB 2 coverage and evidence](https://github.com/pulsarfab/regain/blob/v0.5.4.0/docs/usb2-coverage-spike.md)
for model-specific results and limitations.

For Windows ASCOM and the bundled Alpaca server, use
`Regain-ASCOM-0.5.4.0-win-x64-setup.exe`. Install NINA through
`https://nina-plugins.pulsarfab.com/` or `https://nina-plugins.psf-guard.com/`,
or use `Regain-0.5.4.0.zip` manually. The stable registry entry retains its
existing plugin identity and minimum NINA version. Rust workspace version is
0.5.4; the CameraKit ZIP is a separate diagnostic tool.

[Documentation and supported hardware](https://pulsarfab.com/docs/regain/) ·
[Install and upgrade](https://pulsarfab.com/docs/regain/install.html)

Windows release binaries are signed by StackFoundry LLC. Regain is Apache-2.0;
`regain-zwo` also carries a ZWO MIT notice, and bundled third-party components
retain their own licenses. Existing published assets are unchanged.
