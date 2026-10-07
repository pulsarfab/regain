# Explicit hub coordination

Status: development core on `codex/regain-hub`. This is not yet a saved hub
configuration or a frontend feature. Milestone 5 remains open until host/IPC,
generated configuration, native NINA orchestration and acceptance are complete.

## Focuser groups

Use a group when several absolute focusers must follow one logical position with
different steps, directions or offsets. Each member keeps its own target,
transport generation, observed position, phase and error. A completed group means
every member reported stopped at its exact target. An acknowledged Move alone is
not completion.

The current Rust API accepts already connected, resolved `FocuserSession`s. It
uses the same source actors, generation fences and control leases as ordinary
typed drivers. Construction and reading operation status perform no source I/O.
It does not add a device crate, transport or standalone process.

### Calibration and admission

For each member:

```text
device target = round(logical target × scaleNumerator / scaleDenominator) + offset
```

The denominator is positive; the numerator is nonzero and may be negative.
Rounding uses nearest integer with ties away from zero. Integer arithmetic avoids
floating-point calibration drift. The result must fit Int32 and the member's
configured inclusive travel bounds. Logical bounds, all transformed targets,
unique nonempty source IDs and configuration bounds are checked before I/O.

A group has 2–32 distinct sources, a finite 0.01–300 second operation bound and a
0.01–10 second completion observation interval. The deadline starts at admission,
including time before the owned task runs. It does not alter source poll/recovery
settings or shorten the source's existing in-flight mutation deadline.

The controller reserves every member's existing command lease before any Move.
All members must pass live preflight: absolute positioning, valid capabilities,
stopped motion, current/target travel, per-move `MaxIncrement`, and temperature
compensation disabled when supported. It never disables compensation, homes,
reconnects, chunks a large movement or probes Halt implicitly. Overlapping groups
fail Busy rather than wait for one another's partially held leases.

Every member is checked again immediately before its dispatch. External software
or physical changes can still change equipment outside Regain's command leases;
preflight does not provide an atomic hardware transaction.

### Dispatch, cancellation and partial results

Moves are dispatched once in configured order. The controller stops dispatching
later members after rejection, ambiguous acknowledgement, explicit cancellation
or deadline. Motions already acknowledged may overlap; this provides no hardware
synchronization guarantee. Started siblings continue to be observed after a
member fails. Each member polls independently, so a hung read cannot delay a
healthy sibling's later completion publication.

The task owns its leases and status independently of a waiting frontend. Closing
or dropping a waiter does not stop it. Explicit cancellation stops further work,
without implying Halt or rollback. If cancellation/deadline arrives during a
dispatched mutation, the task first receives its bounded acknowledgement or
uncertain result. That can extend return beyond the group deadline by the
remaining source operation time, including residence behind already queued actor
work. This is not a promise of return within one backend timeout. It preserves the distinction between a command
that started and one that was never attempted.

Member phases are `notStarted`, `rejected`, `moving`, `complete`, `failed` and
`uncertain`. Group phases are `preflight`, `moving`, `complete`, `preflightFailed`,
`partialFailure`, `cancelled` and `deadline`. A terminal cancellation/deadline may
retain `moving` members: inspect the equipment explicitly before commanding it
again. Lost replies and generation changes remain uncertain, and the underlying
source actor prevents mutation replay. There is no automatic retry or rollback.

Status uses one latest immutable report with stable operation/group IDs and an
increasing publication sequence. Reads do not poll sources or advance sequence.
Handles can subscribe to changes or await a terminal report.

### Acceptance and remaining integration

Private actor tests cover calibration/reverse/rounding/overflow, preflight across
all members, live limit changes between dispatches, shared lease contention,
overlapping groups, cancellation before admission work and during a held Move,
dropped waiters, exact completion, ignored commands, stalled members, deadlines,
lost acknowledgement without replay, independent sibling observation and source
retirement. No physical focuser or installed vendor driver is used.

Next integration must resolve physical leaves through virtual aliases, include
group definitions in the generated configuration contract, retain host activity
and configuration revision through the owned task, retain a bounded operation
inventory for IPC reattachment, and define explicit shutdown/recovery behavior.
Expose the same operation/results in native NINA. These are required work, not
implied by the core tests. Synchronized camera orchestration, measured start skew,
per-camera results and explicit abort/continue policy remain separate requirements.
