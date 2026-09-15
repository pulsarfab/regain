ZWOgain 0.2.0.0 adds ASI6200MM Pro P25 support and fixes camera recovery.
The SDK remains the default; direct capture is optional.

Current direct cameras: **ASI676MC**, **ASI2600MM Pro (non-P25)**,
**ASI6200MM Pro P25**, and **ASI220MM Mini guide camera**.
**Coming soon:** **ASI2600MM Pro P25** and **ASI6200MM Pro (non-P25)** direct support.

- Adds direct ASI6200 P25 capture, defect correction, bins 1–4, cooling, dew heater, fan, and LED controls.
- Fixes incomplete ASI6200 readout by waiting for all sensor rows before freezing the frame.
- Restores idle camera controls after an abort or failed capture, without starting another exposure.
- Keeps completed images when a later stop/reset fails.
- Gives direct downloads and retries their full timeout budget.
- Finds the selected camera by serial even when another camera of that model is busy.
- Adjusts NINA image areas to the direct driver's size and alignment rules.
- Fixes ASI676 read-retry counts and shortens the README.

The default limit for taking a replacement exposure is still **30 seconds**.
Same-frame rereads have a separate limit and can recover longer exposures.

ASI6200 P25 testing includes dark frames in SDK and direct modes, transfer faults,
cooler recovery, and a full 1,200-second direct exposure in NINA. The latest
recovery fixes passed automated fault and NINA interface tests; they have not
yet been retested on the physical camera. The 2,000-second maximum and recovery
after cable removal or power loss remain untested.

**Install:** add `https://nina-plugins.psf-guard.com/` as a NINA plugin source and
update ZWOgain, then restart NINA. For manual installation, close NINA and
extract `ZwoGain-0.2.0.0.zip` into `%LOCALAPPDATA%\NINA\Plugins\3.0.0\ZwoGain`.
Requires Windows x64, NINA 3.2.0.9001 or later, and the ZWO Windows camera driver.

The release also includes `ZwoGain-CameraKit-0.2.0.0-win-x64.zip`. Extract the
whole ZIP and run `ZwoGain-CameraKit.exe` to collect data for a new camera model.

ZWOgain binaries are signed by StackFoundry LLC. The project is Apache-2.0 and
is not affiliated with or supported by ZWO. Bundled software retains its own licenses.
