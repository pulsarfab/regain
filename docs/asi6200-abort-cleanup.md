# ASI6200 cancellation cleanup

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

## Validation and remaining acceptance

Register-model tests exercise cancellation during synchronization, before and
after each gate, during restoration, at the one-second boundary, and followed
by another synchronization. They also cover USB/environment errors, stale
gates with cleared DDR, ignored gate writes, thermal flag preservation and
the observed SDK command order. These tests run without hardware:

```powershell
cargo test -p regain-zwo --lib asi6200 --locked
```

An ASI6200 is not attached to the development machine for this change.
Physical acceptance remains: cancel repeatedly during arming/synchronization,
around the 0.6/0.8/1.0-second clock gates, during readout and during transfer;
then capture another frame using the same worker. Repeat on original and P25
hardware, with cooling/heater enabled, checking unchanged thermal flags,
successful new synchronization and complete fresh pixels. A captured USB trace
and the new diagnostics should distinguish a handshake failure from stale
gates. The existing transfer-cancellation tests alone do not cover this path.
