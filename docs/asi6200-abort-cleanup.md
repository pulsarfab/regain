# Direct camera cancellation and thermal cleanup

## Incident and evidence

The 2026-10-10/11 Starfront NINA log uses Regain 0.6.0.3 in direct USB mode
with an ASI6200MM Pro. Eight earlier cancellations were followed by successful
captures. At 06:18:30, a requested 15-second exposure was cancelled about
0.17 seconds after capture started. The next capture failed with
`ASI6200 long exposure synchronization failed`, before image download.
All 128 attempts across the remaining 32 capture requests failed the same way.
The log does not contain the camera's register state or enough phase detail to
establish the exact point of that cancellation. Camera revision is also unknown.

Source inspection identified two cancellation gaps in the shared original/P25
ASI6200 acquisition path:

- A cancellation checkpoint between the high and low edges of the FPGA
  synchronization pulse could leave the handshake unfinished.
- Cancellation or a USB/environment error during integration skipped clock
  restoration. Ordinary stop cleared DDR and froze the sensor, but did not
  clear the long-exposure gates. `BC23=1` alone therefore did not establish
  that the camera was ready to synchronize another exposure. Initialization
  did not reset `BD0b`, and deliberately preserved `BD19` for cooling.

These are confirmed software gaps and plausible contributors to the incident.
They are not a hardware reproduction of the remote failure.

## SDK comparison and change

See the [direct camera register reference](camera-usb-registers.md) for request
notation, shared masks, timing constants and the limits of the SDK evidence.

The saved SDK 1.41 P25 Camera Kit trace `20260915T012106Z-31f0a25c`, exercise
`exposure-2000000us`, records this clock-restoration order:

| Operation | Purpose |
| --- | --- |
| Clear `BD19` bit 0, wait 100 ms | Release the FPGA clock gate; preserve thermal flags |
| Write `B6:019e=1`, wait 100 ms | Resume the sensor clock |
| Clear `BD0b` bit 4, then bit 0 | Release the remaining long-exposure gates |

Independent inspection of SDK worker RVAs `1ed526..1ed60b` shows that its
integration cancellation branch still executes this sequence. Its
synchronization loop at `1ed2cc..1ed3fe` completes before that cancellation
check. The SDK DLL SHA-256 is
`0c8778c3cce2012961b079e3c7d0d8348a8b3823939335d9e98148cb5d5dc34a`.
Private USB traces and SDK binaries are not embedded in the tests.

Regain now completes synchronization before observing cancellation, restores
clocks on every exit from the host-timed exposure path, and verifies `BC19`
bit 0 and `BC0b` bits 0/4 are clear. Stop also repairs stale gates before
freezing the sensor and discarding DDR. A failed restoration/readback is an
uncertain acquisition failure, never an acknowledged clean cancellation.
Cancellation arriving during setup is checked again before starting exposure.

This defers cancellation across the bounded handshake and 200 ms restoration
sequence, rather than waiting for an entire short exposure. USB request latency
can add to those delays. No replacement exposure is started by cleanup.
Cooler, heater and unrelated register flags are preserved.

On exposure-path failure, the info-level `capture.clock_cleanup` diagnostic
includes the phase, exposure duration and `BC00/0b/19/23` register snapshot in
its message so it survives forwarding into the NINA log. Unreadable registers
are recorded as unavailable; cleanup still runs.
`capture.stale_clocks` records gates discovered by stop, including those left
by a previous worker.

## Shared fixes across supported cameras

The investigation prompted an audit of every supported direct camera path.
The fixes run in the Rust backend shared by NINA, native ASCOM and Alpaca;
they require no frontend-specific setting.

| Camera path | Cleanup and cancellation behavior |
| --- | --- |
| ASI2600 original/P25 and ASI6200 original/P25 | Shared synchronization and clock restoration; complete each pulse before observing cancellation, restore gates on all exits, verify readbacks and repair stale gates during stop |
| ASI585MM Pro, ASI662MC and ASI676MC | Shared Bayer wake-before-stop for still and video acquisition; verify cleared clock gates and DDR idle before returning |
| Cooled ASI585/2600/6200 | Shared initialization guard reapplies acknowledged thermal settings after initialization and calibration, including failures |
| ASI220MM Mini | Check pending cancellation before arming; its existing unconditional stop remains, without adding another sensor family's clock commands |
| SDK mode | Continue using vendor stop APIs and propagating their errors; direct USB register sequences are not sent alongside the SDK |

ASI2600 and ASI6200 retain distinct SDK register/timing profiles: sensor
standby registers `01ee` and `019e`, long-integration compensation 400 and
200 ms, and one-second compensation 205 and 200 ms. ASI2600 SDK worker
`14c551..14c55c` branches to restoration at `14c5a0`, independently confirming
the required cancellation behavior. Bayer cleanup follows its own traced
200 ms wake sequence; it does not use either model's sensor standby register.

Every clocked model attempts its ordinary full stop even if clock restoration
fails. A failure of either step prevents a clean abort acknowledgement;
two failures retain both error messages.

Hardware testing also found that ASI6200 initialization cleared the heater
enable bit while its cached getter still reported enabled. Skipping explicit
thermal defaults was insufficient. The initialization guard now restores
current output and supported actuator settings and verifies hardware state.
It preserves the setpoint, regulator history and feedback deadline, so rapid
short frames cannot indefinitely delay temperature control. Reconnect keeps
its separate deadline-reset behavior. Disconnect disables cooler and heater.

## Validation and hardware acceptance

Register-model tests exercise cancellation during synchronization, before and
after each gate, during restoration, at the one-second boundary, and followed
by another synchronization. They also cover USB/environment errors, stale
gates with cleared DDR, ignored gate writes, thermal flag preservation and
the observed SDK command order. These tests run without hardware:

```powershell
cargo test -p regain-zwo --lib --locked
```

On 2026-10-11, an attached original ASI6200MM Pro (revision register 3, USB 3)
passed twelve active cancellations: immediate, 170 ms after start,
initialization, exposure start, synchronization, integration, 620/820/1020 ms
into integration, clock restoration, sensor readout and downloading.
Every cancellation ended in the cancelled terminal state and was followed by
a fresh 9576 × 6388, 16-bit, two-second exposure in the same raw worker.
No SDK, worker replacement, device reopen, retained-read recovery or
replacement exposure was needed. Stop acknowledgements took 0–391 ms;
the zero value reflects the host timer's resolution.

Passive USB tracing independently verified `BC19` bit 0 and `BC0b` bits 0/4
clear, and `BC23=1`, before each acknowledgement. The 1020 ms case reached
both long-exposure gates before cancelling, then visibly executed the full
restoration sequence. Full-frame freshness used alternating offset 50/100,
paired baseline frames and median checks on every sensor row; envelope
validation alone cannot exclude stale rows. Each follow-up also had different
pixels from its predecessor.

Two targeted comparisons with the signed 0.6.0.3 worker (170 ms after start
and 1020 ms after exposure start) also captured successfully afterward.
Its trace nevertheless confirms the missing cleanup commands: the early
cancellation armed an exposure and interrupted a synchronization pulse;
the later cancellation set both gates and acknowledged DDR idle without
clearing or reading back those gates. The fixed worker completed the pulse
or cancelled before arming, and cleared/read back the gates before returning.
The remote lockup has therefore not been reproduced on this original camera.
P25 physical acceptance remains outstanding.

After sharing the implementation, the original ASI6200 repeated the 170 ms,
1020 ms gated-integration and downloading cases with fresh full frames and
physical thermal readbacks. The NINA camera API acceptance also passed caller
cancellation at 170 ms and explicit `AbortExposure` after two seconds, each
followed by a new two-second image. Supervisor and raw worker IDs stayed
unchanged; recapture and USB-read retry counts remained zero. This exercises
the actual plugin API and Rust supervisor with a test image factory, rather
than automating the NINA UI.

An attached ASI662MC also passed early and gated still-exposure aborts with
physical gate/DDR readbacks and fresh two-second frames in one worker.
Its gated 15-second video exposure then passed `Video::stop`, with clock gates
clear and `BC23=1` read after the final stop command, before acknowledgement.
A fresh two-second still capture followed in the same worker.
The remaining models have SDK-sequence and register-model coverage; their
updated cleanup paths have not been physically retested in this acceptance.

The opt-in harness pins the camera serial and controls only its own worker:

```powershell
python scripts/inspection/validate_6200_abort.py --hardware `
  --worker target/debug/regain-device.exe --serial <camera-serial> `
  --output artifacts/asi6200-abort-acceptance --thermal
```

Use an inspection Python environment with Frida and NumPy. Each run requires
a new output directory and an idle camera. Results include per-row statistics,
hashes, phases and a passive USB trace. Keep raw traces private: they contain
device paths and serials. The existing transfer-cancellation tests alone do
not cover this exposure-abort path.
