# PulsarFab regain 0.6.1.6

Recover an ASCOM mount that unexpectedly stops tracking through NINA, using your
existing installed mount driver and NINA's normal coordinate and centering paths.

- **Regain mount provider** wraps NINA's ASCOM telescope adapter without opening
  another serial connection. It monitors unexpected tracking loss, unavailable
  position and uncommanded coordinate jumps. Explicit tracking stops disarm it.
- **Recover Regain mount** action and **Recover Regain mount on tracking loss**
  trigger stop guiding, home, return to the saved sequence target, restore the
  last healthy tracking rate, plate-solve/center, and restore previously active
  guiding. Automatic recovery checks before the next light; an active exposure
  is allowed to finish.
- **Durable recovery fencing** prevents uncertain home/slew replay after failure,
  cancellation, timeout, sequence reload or NINA restart. Inspection is required
  before another attempt, and an in-flight vendor task keeps ownership until it
  actually ends. Failed recovery stops subsequent acquisition without deadlocking
  NINA's sequence interrupt.
- Tracking verification allows NINA's cached ASCOM telemetry to refresh after a
  write, without repeating the command. Invalid trigger settings stop imaging,
  and disconnect faults retain useful diagnostic logging.

Installed NINA 3.2.0.9001 simulator acceptance verified tracking loss during a
full-frame 30-second light, home/return/real local plate solving and centering,
and the next light capture. Injected solver failure stopped subsequent capture;
reloading an older unfenced sequence stayed blocked; explicit inspection cleared
the journal. Complete Core and NINA suites passed (178 and 625 tests; five
hardware/COM opt-in skips). Guiding lifecycle is covered by mediator fixtures;
physical AM7 homing, live guiding and observatory safety acceptance remain
outstanding. Enable mount recovery only after validating your own equipment.

See [mount recovery setup and limits](https://github.com/pulsarfab/regain/blob/v0.6.1.6/docs/ascom-mount-recovery.md).
Use `Regain-0.6.1.6.zip` for NINA or
`Regain-ASCOM-0.6.1.6-win-x64-setup.exe` for Windows ASCOM and Alpaca. Mount
recovery is a NINA provider and sequence feature; it adds no native ASCOM mount
Chooser entry or Alpaca telescope endpoint.

This is a managed-plugin revision. Rust crates remain at the already published
`0.6.1`; no new crates.io upload is required. Existing camera, USB recovery,
accessory and Hub features remain included.
