# Regain Hub implementation plan

Status: implementation in progress on `codex/regain-hub`; one PR against main.
Maintenance releases remain on `release/0.5`. Do not merge the hub PR until all
milestone gates and the final completion audit pass.
Last updated: 2026-10-05.

Use this document as the working checklist. Complete one reviewable milestone at
a time, record its tests and remaining limitations, and update the next action
before moving on. Keep shipped documentation distinct from planned capabilities.

## What do you want to do?

| Use case | Hub behavior |
| --- | --- |
| See switches and gauges from several devices together | Combine selected channels from native, Alpaca, and Windows ASCOM sources into one Switch device, retaining units, bounds, and write permissions. |
| Use several safety monitors in NINA | Publish one safety decision with per-source diagnostics, bounded communication grace, and confirmed recovery. |
| Build one weather device from several sensors | Select a source for each ObservingConditions measurement, with explicit fallback and freshness rules. |
| Reach an existing Windows ASCOM driver over the network | Republish it through Regain Alpaca using an isolated Windows COM host. |
| Use these combinations directly in NINA | Select a native Regain Hub provider backed by local IPC; no ASCOM output or HTTP listener is required. |
| Share access to equipment across applications | Share source ownership and telemetry while arbitrating commands and acquisition. |
| Coordinate several focusers or cameras | Use explicit coordination policies and report each member's outcome; independent devices remain available. |

## Architecture decisions

```text
Native Regain workers     Remote Alpaca     Windows ASCOM COM host
          \                    |                    /
           +-------- shared source registry -------+
                                |
                     regain-hub library/runtime
                 typed devices, policies, configuration
                                |
                +---------------+----------------+
                |               |                |
           Native NINA     Alpaca server     Native ASCOM
             local IPC         HTTP           local IPC
```

- Add one `regain-hub` crate with typed modules for configuration, sources,
  switches, safety, weather, and later coordination. Do not add a crate for each
  device class. Extract a separate configuration crate only when actual reuse
  and dependency boundaries justify it.
- Keep camera recovery in `regain-core`, hardware protocols in the vendor crates,
  and hardware dispatch in `regain-device`. Reuse their existing ownership and
  recovery behavior rather than introducing a second hardware control path.
- Keep the HTTP frontend in `regain-alpaca`. Share the hub engine with the native
  NINA and ASCOM outputs; those outputs must not depend on an Alpaca round trip.
- Select the executable/host arrangement in milestone 0. Prefer existing shared
  executables or explicit host modes; do not create per-device hub executables.
- A configured source has one owner in a hub runtime. Clients receive connection
  leases; the last lease releases only connections owned by this runtime.
  Cross-process discovery and ownership arbitration must be explicit.
- Windows COM imports run in an isolated host with the driver's required
  bitness/apartment model. A hung driver must not block unrelated devices.
  Linux/macOS can consume an exported Windows device through Alpaca.
- Persist immutable source/output IDs and stable device/channel mappings. Renaming
  or reordering configuration must not silently retarget an existing client.
- Reject dependency cycles, direct self-proxying, and identifiable aliases back to
  the same exported device. Bound requests even when a remote loop is undetectable.
- Use capabilities per device class, not a universal trait that hides important
  protocol differences. Preserve upstream error meaning and source identity.

### Command and camera ownership

Shared connections do not imply permission for concurrent commands. Serialize
writes and give acquisition an explicit owner. Observers can read cached state
without starting or cancelling another client's acquisition. Disconnecting one
client must not disrupt the others.

Do not automatically retry an uncertain exposure start, movement, or power
operation. Reconcile state where possible and report uncertainty otherwise.
Proxying an SDK/ASCOM camera does not grant Regain's direct USB retained-frame
reread capability.

Independent cameras remain separate devices. An ASCOM Camera returns one image;
synchronized multi-camera acquisition belongs in explicit orchestration, with
per-camera images and timing results. Likewise, moving two focusers together is
not independent autofocus on two optical trains.

## Safety behavior

Use the Field Kit state machine as the behavioral reference, adapting its input
sampling to native, Alpaca, and COM sources. Review licensing/attribution before
copying code. The reference implementation is a starting point, not evidence of
Regain hardware acceptance.

Pinned reference: Field Kit commit
[`8be3d38f0b04fa78d7ae36b460ed10656f259d0f`](https://github.com/theatrus/nina-field-kit/tree/8be3d38f0b04fa78d7ae36b460ed10656f259d0f),
especially [the safety design](https://github.com/theatrus/nina-field-kit/blob/8be3d38f0b04fa78d7ae36b460ed10656f259d0f/docs/alpaca-safety-monitor.md),
`EndpointSafetyState.cs`, `SafetyConfiguration.cs`, and the safety tests.

- Every enabled source is required. Empty configuration, startup, invalid data,
  and permanent configuration/authentication faults produce unsafe.
- Track the raw reading, communication state, effective permission, and reason
  separately. Keep failed-cycle, unsafe-observation, and safe-observation counters
  independent. Count new polling observations, never getter calls or retries.
- Explicit unsafe withdraws permission immediately by default. A configurable
  confirmation count may preserve previously established permission briefly,
  but must not refresh the last safe evidence.
- Transient failures may retain previously established safe permission for
  bounded grace. Withdraw permission at the first failed-cycle threshold or
  safe-evidence expiry. Old cached safe data can never restore lost permission.
- Measure safe evidence age from the request start using monotonic time. Enforce
  expiry independently of polling/backoff and reevaluate it on every status read.
- Return to safe requires both consecutive safe readings and a recovery hold.
  A failed attempt clears recovery progress. Only a valid observation can finish
  recovery; reading status cannot do so.
- Aggregate with AND and no hidden second debounce. Grace can preserve an already
  safe aggregate, but cannot establish or restore it; restoration requires every
  source to have fresh, confirmed safe evidence.
- Poll sources independently with bounded concurrency. Reject late responses from
  obsolete requests, connection generations, or configuration revisions.
- Never persist live safe permission. Restart, reconnect, relevant configuration
  replacement, and resume after sleep must invalidate evidence appropriately.
- Keep the frontend device available while upstream sources fail so clients can
  observe unsafe. A standalone connection test must not seed live permission.
- Show source age, raw/effective state, counters, reason, and next retry/recovery
  progress. Export diagnostics without credentials.

Initial defaults inherited from Field Kit, subject to acceptance testing:

| Parameter key | Default |
| --- | --- |
| `pollSeconds` | 30 |
| `requestTimeoutSeconds` | 1 |
| `attemptsPerCycle` | 3 total attempts |
| `failedCyclesToUnsafe` | 3 |
| `unsafeReadingsToUnsafe` | 1 |
| `safeReadingsToSafe` | 3 |
| `maximumSafeAgeSeconds` | 90 |
| `returnToSafeHoldSeconds` | 10 |

Show effective timing in setup. Three safe observations at 30-second intervals
take at least 60 seconds from the first observation, even with a 10-second hold.
Validate relationships such as poll interval plus timeout being shorter than the
safe-evidence lifetime. Preserve bounded backoff and jitter; HTTP success still
requires a valid Alpaca response and Boolean value.

Separate source transport/sampling settings from each safety hub's policy.
Sharing one source must not force two hubs to use the same safety thresholds.
Define sample sequence IDs, cadence requirements, and per-consumer counting so a
faster shared poller cannot accidentally shorten another hub's confirmation time.

## One configuration definition

Typed Rust configuration plus attached descriptors is the authoritative contract.
Generate machine-readable metadata and frontend contracts from it, instead of
maintaining independent lists of strings and ranges in JavaScript and C#.

Each parameter describes:

- Stable key, type, default, optionality, and scope (source, virtual device, or
  source membership policy).
- Label, description, group/order, units, enum choices, bounds, and input step.
- Declarative visibility/enablement conditions and capability requirements.
- Apply behavior: live, reconnect, or restart; sensitive changes must not silently
  reinterpret an active safety session or acquisition.
- Secret/reference handling, export behavior, schema version, and migrations.

Use a small typed condition vocabulary, not arbitrary frontend expressions.
Keep runtime telemetry and capability discovery separate from persisted values.
Retain existing saved keys where compatible; migrate deliberate changes.

The engine exposes versioned equivalents of `describeConfig`, `validateConfig`,
and revision-checked `applyConfig`. Validation returns field-addressable errors,
including cross-field constraints. Apply atomically; failed validation leaves the
running configuration intact. Reject incompatible client/schema versions clearly.

Generate JSON Schema and C#/web contracts as appropriate. Each frontend may use
native layouts and specialized controls, but consumes the same meanings, units,
defaults, and validation. Preview effective configuration before applying it.
Credentials use references and are redacted from logs and ordinary exports.

First adoption is hub safety/source configuration. Then migrate existing camera
recovery settings incrementally: the Alpaca setup UI and native ASCOM setup
currently duplicate labels and bounds. Avoid a repository-wide UI rewrite.

## Milestones and acceptance gates

### 0. Confirm boundaries and contracts

- [x] Reconcile this plan with current main before implementation; inspect existing
  camera supervisors, accessory leases, slot persistence, and frontend IPC.
- [x] Choose the shared runtime host, launch/discovery protocol, ownership scope,
  and local IPC access rules. Specify behavior when two frontends launch at once.
- [x] Define source identity, capabilities, observations, command leases, errors,
  virtual-device IDs, and configuration envelope/revisions.
- [x] Define compatible COM host bitness/apartment handling and driver isolation.
- [x] Pin upstream interface versions and Field Kit behavior tests; review reuse
  licensing. Record any intentional deviations here.

Gate: reviewed contracts plus example configurations for a mixed switch hub and
a two-source safety hub, with no hardware needed.

Contracts are in [hub-contract.md](hub-contract.md). The mixed-switch and
two-source safety examples pass executable configuration tests, including wire
round trips and reorder preservation. Milestone 0's gate has passed. Review found that Field Kit retains
safe permission across some edits; Regain intentionally clears it on relevant
configuration changes, as this plan originally specified.

### 1. Shared configuration and safety engine

- [x] Add the shared crate/modules and descriptor generation.
- [x] Implement configuration validation, migrations, redaction, and atomic apply.
- [x] Implement pure safety state transitions against an injectable monotonic clock.
- [x] Add independent expiry, observation sequencing, and revision fencing.
- [x] Build deterministic fixtures for startup, unsafe, grace, expiry during hung
  polling/backoff, failed recovery, late responses, restart, and sleep/resume.
- [x] Verify multi-source AND, independent policies, no getter-based counting, and
  identical metadata/defaults in generated frontend contracts.

Gate: deterministic policy tests pass; malformed and conflicting configuration
cannot change live state; no frontend reimplements safety decisions.

Current checkpoint: `regain-hub` has typed source/output configuration, a persisted
identity ledger, revision-checked atomic replacement, policy parameter descriptors
and JSON Schema, and the pure safety engine plus independent expiry runtime.
Schema 1 is the first hub schema: absent/future versions fail explicitly; existing
camera profile migration remains in milestone 4. Configuration examples and tests
perform no hardware I/O. Policy descriptors generate defaults, labels, units,
ranges, and validation from one declaration.

Milestone 1's foundation gate has passed. Source/output schema and descriptions
are generated from the Rust types; policy schema comes from the same declarations
as defaults and numeric validation. `contracts/hub-config.json` is the generated
contract fixture; the live host will serve the same description with its actual
capabilities. Generic JavaScript and .NET readers consume its tagged choices,
conditional fields, capability gates, labels, defaults, units, and read-only
identity rules. They do not duplicate equipment keys or safety policy.
Independent JSON Schema validation and both reader tests passed. Local Rust
1.89.0 compatibility, standalone packaging, multi-source AND, independent
membership cadence, and getter/retry counting also have tests.
The source poll scheduler will attach its real attempts/backoff to these tested
events in milestone 2. The host must call generation reset on resume and relevant
configuration changes; the engine's reset behavior is tested, not yet wired to OS
notifications. The final product gate still requires actual frontend behavior.

Review findings fixed in this checkpoint:

- Persist retired device/channel identity assignments so removal and restart do
  not permit number reuse or silently retarget an existing channel.
- Invalidate permission on runtime drop and serialize that publication with the
  expiry task, preventing a last cached safe snapshot from surviving shutdown.
- Use per-membership confirmation cadence so a fast shared poller cannot shorten
  another safety output's confirmation thresholds.
- Reject invalid observation timestamps, retired generations, duplicate sequences,
  credential-bearing URLs, dependency cycles, and conflicting config revisions.

### 2. Source registry and first Alpaca vertical slice

- [ ] Implement native worker adapters and bounded Alpaca source adapters.
- [ ] Persist stable mappings, reject duplicates/cycles, and share connection leases.
- [ ] Implement combined Switch channels with immutable IDs, access flags, units,
  ranges, step validation, and channel provenance.
- [ ] Implement SafetyMonitor and ObservingConditions; define per-measurement
  freshness, fallback, units, and averaging semantics before exposing weather.
- [ ] Expose these virtual devices through the existing Alpaca server and setup UI.
- [ ] Test mixed native/network sources, disconnects, slow sources, restart mapping,
  uncertain writes, and two simultaneous clients.

Gate: one server publishes a working mixed-source switch hub, safety hub, and
weather hub; unrelated devices remain responsive during an upstream failure.

### 3. Windows imports and native NINA — first useful release

- [ ] Implement isolated COM import with timeouts, explicit connection ownership,
  cached telemetry, and recovery from a hung driver host.
- [ ] Add native NINA Switch, SafetyMonitor, and ObservingConditions providers and
  configuration UI using shared descriptors and local IPC.
- [ ] Exercise native, Alpaca, and COM inputs through the same policies.
- [ ] Verify NINA operation with no Alpaca listener and, for native/network-only
  sources, no dependency on ASCOM Platform.
- [ ] Verify source sharing between NINA and Alpaca clients, including disconnect
  order, restart, and competing command attempts.

Gate: NINA sees combined channels and dependable safety/weather status; a stalled
COM driver cannot prevent cached safety evidence from expiring. Document exact
tested NINA/ASCOM versions and supported imports.

### 4. Native ASCOM outputs and broader republishing

- [ ] Add native ASCOM hub outputs with the shared setup styling and descriptors.
- [ ] Preserve dynamic output identity and selection across registration/upgrades.
- [ ] Extend typed proxy coverage to focusers, rotators, filter wheels, flat panels,
  and cameras in separately reviewable increments.
- [ ] Specify camera buffer lifetime, image transport, capability passthrough, and
  acquisition ownership before enabling camera proxies.
- [ ] Run relevant ASCOM/Alpaca conformance checks and multi-client failure tests.
- [ ] Migrate existing camera recovery configuration metadata without changing its
  saved behavior or claiming proxy cameras support retained-frame rereads.

Gate: all three outputs use the same engine/configuration; conformance and image
integrity tests cover the classes shipped, with limitations stated explicitly.

### 5. Explicit coordination

- [ ] Add focuser groups with calibrated transforms, offsets, per-device limits,
  preflight checks, cancellation, and visible partial-failure results.
- [ ] Define synchronized camera orchestration with separate results, measured
  start skew, partial failures, and explicit abort/continue policy.
- [ ] Expose useful orchestration in native NINA without pretending one standard
  Camera interface can return several independent images.

Gate: simulation/fault-injection tests precede hardware trials; measured behavior
and timing limits are documented. Do not claim rollback or hard synchronization
when the underlying hardware cannot provide them.

## Working and release checklist

For each milestone, keep changes small enough to review independently:

- [ ] Record implemented scope and deviations from this plan.
- [ ] Run relevant unit/integration checks and fault scenarios; record commands
  and evidence. Use simulation first and label it clearly.
- [ ] Perform applicable frontend/conformance and real-device acceptance checks.
- [ ] Review lifecycle, cancellation, stale-data, migration, and ownership behavior.
- [ ] Update README and `/docs/regain/` documentation for shipped capabilities,
  including setup examples, supported source/output combinations, and screenshots.
- [ ] Record PR/commit, checks, remaining issues, and next action below.

Publishing installers, plugin feeds, and releases follows the existing
[release process](releasing.md) when that milestone is selected for release.
Keep existing device profiles and registrations compatible throughout migration.

## Progress log

| Date | Work | Evidence / next action |
| --- | --- | --- |
| 2026-10-05 | Captured agreed hub architecture, Field Kit safety semantics, shared configuration contract, and incremental gates. | Planning only. Next: milestone 0, beginning with reconciliation against current main and runtime ownership/IPC design. |
| 2026-10-05 | Reconciled main `c8fd7c4`; reviewed native camera pipes, stable slots, serial COM sharing, Field Kit source/license, and installed interface declarations. Recorded shared host/IPC, COM isolation, identity, configuration, and safety contracts. | Milestone 0 contracts reviewed; executable config examples and milestone 1 implementation next. No hub frontend support is shipped yet. |
| 2026-10-05 | Milestone 0 gate passed; implemented and reviewed the shared configuration/safety foundation. | `cargo test -p regain-hub --locked`: 27 passed. `cargo clippy -p regain-hub --all-targets --locked -- -D warnings`: passed. Complete remaining milestone 1 metadata/contracts, then implement the shared source runtime and Alpaca vertical slice. All later milestones remain in scope. |
| 2026-10-05 | Checked minimum Rust version and crate distribution independently of the application. | `cargo +1.89.0 check -p regain-hub --all-targets --locked`, `cargo package -p regain-hub --allow-dirty --locked`, formatting and diff checks passed. Package contains both executable config fixtures and its license. |
| 2026-10-05 | Completed milestone 1's generated configuration description and shared setup readers. Corrected schema/backend Unicode label counting and locked existing device numbers in the readers. | 28 Rust tests, 4 independent schema tests, JavaScript contract tests, 2 native .NET contract tests, net48 build, Clippy, and Rust 1.89.0 check passed. Next: shared source registry, polling/transport adapters, then the Alpaca vertical slice. PR #21 remains draft; all later gates remain required. |
