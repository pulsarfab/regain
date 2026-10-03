# Direct cameras on USB 2

The direct backend accepts USB 2 **high-speed** connections for all of its
supported camera identities: ASI662MC, ASI676MC, ASI2600MM Duo main, ASI2600MM
Pro P25, ASI6200MM Pro (original and P25), and ASI220MM Mini. This is a transport
fallback on the same selected camera, not an SDK fallback. The bus negotiates
the connection; Regain does not cycle a port or change driver bindings to force
USB 2. The SDK backend remains available and unchanged.

## What changed

Previously, open and acquisition required `bcdUSB == 0x0300` except for the
USB 2-native guide camera. A real ASI662MC on a USB 2 cable reports `0x0210`
with a 512-byte bulk-IN endpoint. It was rejected before serial verification
or sensor initialization with the unhelpful message "camera USB interface
differs from the verified model".

Shared validation now checks the expected ZWO VID/PID and exactly one bulk-IN
endpoint `0x81` in alternate setting zero. It accepts USB 2.0/2.1 descriptors
with 512-byte packets and no SuperSpeed companion, or USB 3.0/3.1/3.2
descriptors with 1024-byte packets and a valid burst value. ASI220MM Mini remains
USB 2-only. Full-speed 64-byte endpoints, missing/ambiguous endpoints, malformed
descriptors and mismatched identities fail before sensor/environment writes.

The descriptor revision is not itself a speed measurement: see Microsoft's
[device descriptor documentation](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/usb-device-descriptors)
and [bulk transfer packet sizes](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/usb-bulk-and-interrupt-transfer).
Successful worker open logs the selected USB transport. Errors report observed
descriptor values, without device paths or serials in the transport diagnostic.

Sensor register tables, frame envelopes, correction, serial verification,
exclusive ownership and bounded transfer/retry/cancellation budgets remain unchanged.
On USB 2 the Bayer cameras use a minimum sensor frame interval of approximately
100 ms for exposures shorter than one second. Frame and shutter-delay lines
are extended equally, preserving integration time. This retains ASI662 pacing
and also protects short ASI676 ROI readout. USB 3 and host-timed long-exposure
timing are unchanged. This is individual-image capture, not maximum-speed video.
Native USB transfers already align their receive buffers to the endpoint's
packet size and accept only the exact expected image byte count. Windows uses
the existing Cypress driver transfer interface. Slow/shared buses can still
exhaust a transfer deadline; partial frames are never accepted as success.

## Validation and limits

The subsequent [coverage spike](usb2-coverage-spike.md) found a missing
link-specific SDK FPGA output-throttle branch. Descriptor acceptance and the
Bayer hardware passes below are not a blanket sign-off for other models.
The spike's later [P25 hardware pass](usb2-coverage-spike.md#p25-follow-up-hardware-result)
adds Windows USB 2 ASI2600MM Pro P25 evidence, including full-row freshness and
production-worker cancellation/reopen.

Operator-authorized Windows tests on 2026-10-03 used local ASI662MC and ASI676MC
cameras, each physically connected through USB 2 in turn. The descriptors
reported `03c3:662b` / `03c3:676d`, `bcdUSB 0x0210`,
endpoint `0x81`, 512-byte packets, a 25-byte configuration, and Windows driver
`0x01020200`. Each command used an exclusive standalone direct worker; the SDK
was absent. No images were saved/uploaded, no installed settings were changed,
and no port resets or power cycles were performed.

| Manual check | Result |
| --- | --- |
| 128 x 128, 100 ms, zero read retries | Complete 32,768-byte frame |
| Three 1920 x 1080 frames, 100 ms, zero retries | Three complete 4,147,200-byte frames; distinct frame hashes |
| Replay of each full frame | All bytes identical; no additional exposure |
| Full frame, 1 s, host read interrupted after 1 MiB | One retained-frame retry; original prefix matched; full replay identical |
| 648 x 482 at (16,16), 32 us, gain 200 | Complete 624,672-byte frame and identical replay, including a non-packet-aligned last transfer |
| Full frame, 30 s, zero retries | Acquisition 30.215 s; complete frame and identical replay |

The table above is ASI662MC. ASI676MC checks additionally passed:

| Manual check | Result |
| --- | --- |
| 128 x 128, 100 ms, zero retries | Complete 32,768-byte frame |
| Three 3552 x 3552 frames, 100 ms, zero retries | Three complete 25,233,408-byte frames, distinct hashes, identical replay |
| Full frame, 30 s, interrupt after 1 MiB, abandon replay after 3 MiB | One retained retry; original prefix matched; restarted replay identical; acquisition 30.888 s |
| Three 648 x 482 frames at (16,16), 32 us, gain 200 | Complete frames and identical replay after pacing correction |
| Three full frames, 32 us, interrupt after 1 MiB, abandon replay after 3 MiB | All complete; one retained retry each; prefixes and replay identical |
| Production pipe worker open/capture/close/reopen/capture | Two complete full frames; cached locator and verified serial reused; no second inventory scan |

An initial unpaced ASI676 32 us ROI experiment failed with "retained-frame replay
differs from original frame". It was rejected, not counted as a pass. The equal
frame/shutter padding above resolved the mismatch in the three-case repeat and
full-frame interrupted-read follow-up. This is evidence for the pacing change,
not proof that every USB bus or firmware fault is recoverable.

These results validate acquisition and deliberate host-read interruption, not
USB bus faults, physical disconnect/reset, optical accuracy, cold power-up,
long exposures beyond 30 seconds, or a complete installed-app SDK-to-direct
handoff. Long frames were saturated under available illumination.

**ASI2600MM Duo and both ASI6200 revisions' new USB 2 paths are not yet
hardware-validated.** ASI2600MM Pro P25 passed the follow-up matrix linked above.
ASI220MM Mini's
existing USB 2 evidence is in [the guide workup](duo-capture.md). Descriptor
fixtures cover all supported identities, USB 2/3 layouts and rejected cases;
simulator success must not be reported as physical model validation. Model-
specific initialization, full-frame capture, ROI/binning, replay, cancellation
and reopen need checks on each available USB 2 camera. Linux/macOS USB 2
hardware remains unvalidated.

## Focused manual probe

With the selected camera idle, this command reads only its standard USB
descriptors. It opens no other model, refuses multiple matching interfaces,
does not load the SDK, and sends no sensor commands or bulk reads:

```powershell
target/debug/regain-device.exe zwo camera-direct --probe-pid 662b
```

Replace `662b` only with another supported product ID (`676d`, `2601`, `260e`,
`620b`, `2209`). The process has a 30-second watchdog. Hardware exercises are
manual and operator-authorized; automated tests/CI use fixtures and simulators.
