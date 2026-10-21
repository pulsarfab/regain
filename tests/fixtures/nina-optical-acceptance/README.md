# Installed NINA optical recovery acceptance

This test-only plugin drives NINA's real camera, filter-wheel, rotator and imaging
mediators. It is inert unless `REGAIN_NINA_OPTICAL_ACCEPTANCE` names an explicit
test directory. It is not included in Regain's release packages.

Use an isolated NINA profile. Back up the installed Regain DLLs and settings,
install the locally built DLLs and this fixture plugin, and select a camera that
is idle and safe for five-second, full-frame RAW16 captures. Do not enable camera
USB reset for this test. Never use a live observing session.

Build this project and `../optical-worker/OpticalWorkerFixture.csproj`. Launch NINA
with these process-local environment variables:

| Variable | Value |
| --- | --- |
| `REGAIN_NINA_OPTICAL_ACCEPTANCE` | Fresh acceptance directory |
| `REGAIN_OPTICAL_FIXTURE_DIRECTORY` | `optical-state` inside that directory |
| `REGAIN_ACCESSORY_WORKER` | Full path to `OpticalWorkerFixture.exe` |
| `REGAIN_CAA_WORKER` | Same fixture executable |
| `REGAIN_ACCESSORY_SETTINGS` | Isolated accessory settings directory |
| `REGAIN_ROTATOR_SETTINGS` | Isolated CAA profile JSON |

Save EFW serial `0102030405060709` in `efw-nina.json` and CAA serial
`0102030405060708` in the isolated CAA profile. These are fixture identities, not
hardware serials. The fixture persists slot 3 and a 30-degree rotator position;
it never opens USB or moves equipment.

Run `scripts/test-nina-optical-recovery.py DIRECTORY --width WIDTH --height HEIGHT`
with the selected camera's full-frame dimensions. The runner checks recovery
before capture, interruption during exposure, rejection of changed optics,
rejection of a disconnected wheel, explicit reconnection, FITS delivery and the
absence of movement or origin-reset commands. An exposure overlapping an optical
disconnect is discarded even when its final position matches; the next fresh
capture must succeed without a reconnect when state is verified unchanged.

Results and five full-frame FITS files remain in the test directory. The JSON
request mailbox uses only a fixed command list; it is not a general shell or
remote-control interface. Restore the installed DLLs, settings and original NINA
profile after testing. Physical EFW/CAA cable removal and long USB absences need
separate hardware validation; this fixture does not establish either.

For a physical CAA-only pass, set
`REGAIN_NINA_OPTICAL_ACCEPTANCE_NO_WHEEL=1` and bind the isolated CAA profile to
the actual hardware serial. Do not set `REGAIN_OPTICAL_FIXTURE_DIRECTORY` or
point either worker variable at `OpticalWorkerFixture`. The
`rotator-state` mailbox command returns a fresh verified optical state and its
connection epoch. Preserve the original angle, logical offset, reverse and limit
settings. A separate, bounded USB interruption fixture or a cable disconnect is
required; the NINA harness itself cannot disable USB devices. An exact downstream
port cycle tests handle invalidation and reconnection, not sustained power loss.
Do not run the injected-worker Python runner against physical accessories.
