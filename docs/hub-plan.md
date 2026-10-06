# Regain Hub implementation plan

Status: implementation in progress on `codex/regain-hub`; one PR against main.
Maintenance releases remain on `release/0.5`. Do not merge the hub PR until all
milestone gates and the final completion audit pass.
This branch uses development version `0.6.0` / Windows `0.6.0.0`; it has not been
tagged or published. Changed cross-crate APIs require an unpublished version so
Cargo package verification uses the new workspace packages.
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

Current checkpoint: the shared source registry validates configuration before
constructing adapters, returns one actor per source ID, and shares connection
leases across outputs. Actors serialize commands through bounded queues, enforce
I/O deadlines, expose cached status independently of I/O, and never replay
uncertain writes. Poll attempts/cycles, connection generations, jitter, and
Retry-After are connected to the safety engine. Each safety output owns its own
policy and leases; shutdown, transport reset, and lost observations invalidate
permission. A loopback HTTP fixture exercises Alpaca through this complete
source-to-policy path. The initial HTTP output adapter is now implemented below;
shared setup and final protocol/hardware acceptance remain pending.

The Alpaca adapter has bounded scalar requests/responses, typed poll samples,
sanitized errors, no redirects or automatic HTTP retries, and explicit external
versus managed connection ownership. It discovers InterfaceVersion and selects
legacy `Connected` or modern per-client Connect/Disconnect/Connecting. Each
handshake step has a request deadline; the whole handshake has a separate
30-second default deadline. Pending steps are not failed safety observations.
Only acknowledged owned connections are released, and ambiguous connection
writes are not replayed. Broader device capabilities and interface conformance
still need implementation and verification.
The host resolves immutable credential references from separate user storage;
missing, corrupt, or inaccessible records fail closed. Windows uses user DPAPI
encryption; Unix uses private plaintext files. Frontends must show the reported
protection method and use the shared write-only credential descriptors.

Typed Switch and ObservingConditions controllers now have local tests. Switch
slots include unavailable tombstones for removed channels, preserve provenance,
and check source write permissions/bounds before dispatch. Writes are rounded to
the ASCOM step, protected by exclusive control, fenced at actual dispatch, and
followed by a fresh poll. Cancellation releases only that operation's lease.
Weather selects/fails over per metric using explicit units and source ages;
partial sensor errors do not erase unrelated measurements. Its bounded history
uses time-weighted averages and circular wind direction; changing source or
generation clears history. The shared schema includes optional source unit
assertions, and the mixed-weather example passes configuration/round-trip tests.

Refinements from interface review: one AveragePeriod applies to an entire weather
output (excluding the already-defined upstream WindGust statistic); humidity and
dew point must be configured together; wind direction requires wind speed for
calm reporting. Unknown-unit channel mappings require an explicit canonical unit;
there are no implicit conversions. Removed switch slots stay within a bounded
0–1023 range, including the persisted identity history.

Multi-property Alpaca polling now budgets each HTTP request separately, including
sensor-age reads, and admits commands between requests. Per-key timestamps and
sequences prevent unrelated samples from refreshing old evidence or adding to an
average. Partial and historical caches have key/text limits. Transient reads
retry the failed key and honor Retry-After. Weather Refresh triggers upstream
acquisition and schedules polling without waiting for measurements; it respects
retry delays and retains sensor ages. Local latency, retry, and cache tests pass.

The shared accessory worker client now lives in `regain-core`; existing Alpaca
CAA/Falcon, EFW/EAF/FocusCube3/ETA, and OFP2 endpoints use it. The common client
bounds messages and retires a child after timeout, cancellation, or invalid
framing. Process fixtures and all seven existing accessory/Alpaca simulation
suites pass. A framed worker error may still follow a dispatched USB command and
must not authorize replay.

The native hub adapter now uses that client for CAA, EFW, EAF, FocusCube3, Falcon,
OFP2, and ETA. It verifies the selected identity, maps scalar telemetry, validates
movement/light parameters before dispatch, and preserves worker motion and
calibration behavior. It does not substitute simulation after hardware failure.
Native temperature can feed Switch gauges and Weather using the same source
leases. A mixed loopback-Alpaca/native-worker test verifies this sharing and a
remote switch write. Camera sources, persistent rotator reference settings, and
the full proxy interface surface remain for milestone 4. Missing temperatures
are per-measurement errors, not a reason to discard other valid telemetry.

The configuration-driven source factory now derives one deduplicated polling
plan per source from all outputs, retains weather sensor ages, enforces the
combined sample limit during construction, and uses the fastest enabled safety
confirmation interval without changing persisted settings or output policy.
Native/Alpaca adapters and credentials are prepared before actors start, with no
device I/O during construction. The mixed-source test uses this factory. The
credential provider now uses protected user storage through the shared host.

The shared output runtime now owns Switch, SafetyMonitor, and Weather controllers
and host-generated client sessions. Clients share safety recovery state and
weather settings; closing one does not close another's sources. Pending connects
are cancellable and fenced against replacement. In-flight commands retain their
leases through client disconnect. Explicit shutdown revokes safety first, stops
new clients, drains all source actors, and preserves uncertain cleanup results
without replay. The mixed native/network test now uses the complete runtime
builder and separate clients rather than constructing controllers itself.

The scalar IPC dispatcher now uses the shared runtime, with a versioned hello,
bounded framing, typed device operations, configuration description/read/validation,
and connection-owned client identities. Requests start in order but can finish
out of order; slow source I/O does not block cached safety reads or EOF cleanup.
Tests cover unknown fields, spoofed client IDs, replayed request IDs, deadlines,
overload, oversized replies, and clients that stop reading.

Protected local endpoints now use per-user named pipes/Unix sockets and a separate
OS ownership lock keyed by canonical configuration path. Accepted connections
retain the lock, atomic config replacement keeps the same identity, and process
death releases ownership. Windows tests exercise anonymous denial, permissive ACL
rejection, competing processes, crash recovery, and real framed IPC. Unix permission,
link, and socket-cleanup tests pass in Linux x64/ARM64 and macOS Intel/ARM64 CI for
endpoint commit `0c8bfe7`.

The shared executable now has `--hub-host --hub-config ABSOLUTE_PATH` mode with no
HTTP/discovery listener. It acquires ownership before preparing sources; a second
launch validates the existing host's hello and exits. A supervisor bounds clients
at 32 and retains ownership through shutdown even if its waiter is cancelled.
Review exposed and fixed repeated last-lease/actor disconnects and Windows pipe
instance exhaustion from retained client handles. Production-executable tests
cover restart, duplicate launch, rejected options/configs, and a network safety
source changing from safe to unsafe over local IPC. Frontend launch/attach and
reconnection are still pending.

The host now applies configuration through revision-checked IPC: stage and validate
before replacement, freeze connection reservations, preserve the old runtime on
pre-commit failure, then drain it before activating the new generation. This
whole-runtime replacement requires all outputs and retained operations to be
disconnected. Existing streams retain their client IDs. Accepted applies survive
RPC cancellation; post-commit uncertainty is visible through getConfig/hostStatus
and blocks device admission when cleanup is uncertain. Production-executable tests
exercise edits, stale revisions, same-stream use, and restart persistence.

The previous host checkpoint's macOS admission-recovery test failed after a queued
client disconnected. Unix acceptance now rejects aborted/unverifiable peers while
preserving the listener. The corrected checkpoint passed Linux x64/ARM64 and macOS
Intel/ARM64 CI, including the directory-flush failure fixture on Unix.

Credential creation/status/deletion now run through private IPC. Configuration
contains references only; records are scoped to the canonical configuration path
and OS user. Rotation creates a new reference, applies it, then deletes the old
reference. Deletion and apply share one update gate, retained through cancelled
requests and shutdown. Configured references cannot be deleted. A real executable
test creates a credential, authenticates loopback Alpaca requests, restarts the
host, authenticates again, and removes the unused record. Credential storage now
passes Linux x64/ARM64 and macOS Intel/ARM64 CI; shared UI integration remains required.

Setup capability inspection is now available over private IPC for Alpaca
Switch/SafetyMonitor/ObservingConditions and native accessories. It uses temporary
leases on the existing actor, bounded Switch pages, strict typed probe results,
shared range/unit/property definitions, and generation-fenced reads. Inspection
participates in apply quiescence and never feeds getter results into safety
confirmation or freshness. Mock faults, real loopback managed connection sharing,
IPC dispatch, and all seven simulated production workers are covered locally.
Broader proxy capabilities, discovery of unconfigured devices, and UI adoption
remain required; this is a setup inspection path, not completed frontend support.

Explicit simulated Switch, SafetyMonitor, and Weather sources now use the same
actors and controllers as real sources, with typed private IPC controls and shared
configuration descriptions. Tests cover shared switch state, read-only sensors,
uncertain writes, stale/absent weather readings, and invalid or timed-out safety.
Source/output diagnostics mark simulation; process restart starts safety unsafe.
Review fixed a safety consumer startup generation race without weakening its
event-loss handling. The executable accepts a checked-in simulated observatory
example without launching hardware workers.

Local validation for simulation: 160 hub tests plus the endpoint process fixture,
15 Alpaca tests, Clippy, Rust 1.89.0, generated-contract freshness, and fresh
transport/core/hub/Alpaca package verification pass. Setup inspection checkpoint
`9ae966e` passed all four portable CI platforms; this new checkpoint awaits CI.

Local virtual sources now bind Switch, SafetyMonitor, and Weather outputs through
internal clients on the same runtime. They reuse typed IPC operations, propagate
simulation labels and scalar ages, and retain safety evidence timestamps. New
policies cannot seed from an already-safe inner output, and repeated virtual polls
cannot accelerate confirmation or extend safe lifetime. Review covered graph
cycles, nested permissions and uncertain writes, cancellation, reference lifetime,
shutdown, and apply quiescence. Production-host tests exercise nested switch writes
and configuration replacement after all internal leases drain. Frontend and
conformance gates remain open.

The Rust frontend client and `--hub-attach` helper now provide bounded multiplexed
requests and explicit shared-host attachment. Tests cover out-of-order replies,
cancelled callers, uncertain writes, malformed frames, resource release, and
production-process restart. Windows launch disables handle inheritance after a
test exposed capture pipes surviving the launcher. Two clients share one host;
host loss requires a new session, and a held ownership lock never causes an
automatic replacement. Local validation passes 177 hub tests plus the endpoint
fixture, 18 Alpaca tests, Clippy, Rust 1.89.0, schema freshness, and fresh package
verification. These APIs still need adoption in the actual frontends.

The existing HTTP executable now attaches to that shared host and publishes
Switch, SafetyMonitor, and ObservingConditions with configured IDs/numbers. Each
connected Alpaca ClientID has a separate bounded IPC session. Current tests cover
scalar mapping, simulation labels, disconnect independence, failed source reads,
uncertain writes, client capacity, and HTTP process death without host death.
The adapter advertises synchronous interfaces (Switch 2, Safety/Weather 1);
modern connection/state interfaces are an explicit remaining refinement.

An initial `/setup/hub` editor now consumes shared Rust schema metadata for fields,
choices, references, units, bounds, defaults, identities, and capability gating.
It keeps a draft and reviewed candidate, validates through the host, applies with
revision checks, and exposes source status/explicit paged inspection. JavaScript
and .NET readers share the additional reference/enum metadata. Tests cover durable
API updates, connection/stale-revision guards, and rejection of cross-origin or
device-command requests. Chrome verified editing, review, and successful apply;
the simulation screenshot is checked in. Credential controls, initialization,
richer diagnostics, and reconnect/resume UI are still needed before setup acceptance.

Next: modern connection/state/error conformance, native frontend attachment,
and the remaining shared setup refinements. Basic frontend error translation is tested; complete
protocol conformance remains unverified. Generic
scalar polling does not establish camera image or acquisition support. No complete
milestone 2 or frontend/hardware gate is closed by these library controllers.

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
| 2026-10-05 | Added shared source actors/registry, bounded Alpaca transport, and safety output subscriptions with independent leases and policies. Reviewed cancellation, connection ownership, retry delays, ambiguous writes, malformed responses, and event loss. | 50 Rust tests now cover the foundation plus actor/network/safety integration. Clippy, Rust 1.89.0 check, standalone package verification, and generated-contract freshness passed. Next: typed switch/weather controllers, native workers, host IPC, and Alpaca publication. Full original milestones 2–5 remain required. |
| 2026-10-05 | Added typed switch/weather controllers, scalar sample status, unit assertions, cancellation-safe leases, partial sensor failures, and dispatch generation checks. Refined stable slots and weather averaging against the ASCOM interfaces. | 63 Rust tests pass, including mixed switch controls/gauges, cancellation, step/permission checks, tombstones, weather freshness/fallback/averaging, and HTTP weather source sharing. Updated schema passes web, independent JSON Schema, and native .NET readers. Next: poll scheduling/budgets, capability negotiation, native adapters, and host IPC before frontend/conformance gates. |
| 2026-10-05 | Split Alpaca polling into bounded requests with per-key evidence, bounded incremental caches, same-key retries, and prompt weather Refresh. Corrected Refresh against the ASCOM interface during review. | 70 local Rust tests, Clippy, Rust 1.89.0, standalone package verification, and generated-contract freshness pass. Latency fixtures exercise command interleaving and Retry-After. Next: shared native accessory transport/adapters, capability negotiation, and host IPC. All frontend and later milestone gates remain open. |
| 2026-10-05 | Extracted the shared accessory worker client into regain-core and adopted it in existing Alpaca accessories. Added cancellation-safe process retirement, framing limits, and process fixtures. Moved development versions to 0.6.0 / 0.6.0.0 after packaging exposed resolution of the published 0.5.10 dependency. | Core tests, Clippy, Rust 1.89.0 checks, and seven production-worker/Alpaca simulation suites pass. Native hub mapping, capabilities, host IPC, and all later gates remain pending. No release or tag was created. |
| 2026-10-05 | Added native hub accessory adapters on the shared transport, covering seven families, scalar telemetry, validated motion/light/calibration commands, explicit simulation identity, and source leases. Added a mixed native/network Switch plus shared Weather test. | 79 Rust hub tests pass with production workers in simulation, including EFW calibration and OFP2 movement/light controls. Clippy, Rust 1.89.0, and generated-contract freshness pass. Windows test entrypoint now supplies its built worker path, matching portable CI. Next: capability/connection negotiation, credentials, host IPC, and frontend publication; all later gates remain required. |
| 2026-10-05 | Implemented version-negotiated Alpaca connections, bounded incremental handshakes, shared connection diagnostics, and a schema-described connection timeout. Review corrected repeated uncertain disconnects and duplicate actor resets on handshake failure. | 89 Rust hub tests pass, including cancellation, pending/slow handshakes, external ownership, and reconnect during asynchronous disconnect. Clippy, Rust 1.89.0, package verification, schema freshness, and web/Python/.NET configuration readers pass. Next: shared host/source construction, protected credentials, capabilities, IPC, and frontend publication. Milestones 2–5 and final acceptance remain open. |
| 2026-10-05 | Added configuration-driven native/Alpaca source construction, unioned poll plans, enabled-membership polling cadence, bounded plan growth, and the credential-provider boundary. Mixed-source integration now uses the real factory. | 95 hub tests pass, including plan deduplication, weather ages, disabled memberships, combined limits, validation before credential access, authenticated requests, and shared leases without secret-bearing diagnostics. Next: shared output runtime, protected storage, virtual/simulated sources, host IPC, and publication. No later gate is closed by source construction alone. |
| 2026-10-05 | Added shared output/client sessions, cancellation-safe connection reservations, and explicit runtime/registry shutdown. Safety policy and weather settings are shared per output; queued commands cannot revive a retired actor. Reviewed last-client cleanup, in-flight writes, and cancelled shutdown. | 104 local hub tests cover shared state, disconnect order, cancellation/replacement, source stalls with independent safety expiry, uncertain writes/cleanup, and resumed shutdown. Mixed native/network integration uses the runtime builder. Next: IPC framing, user-only endpoints, startup ownership, protected storage, and frontend publication; all remaining original gates stay open. |
| 2026-10-05 | Added bounded scalar IPC framing, handshake, typed controller dispatch, configuration reads/validation, and per-stream clients. Review fixed request-start ordering and strict parsing of no-argument commands. | 114 local hub tests pass, including ten framed-stream integration/fault cases; Clippy, Rust 1.89.0, and package verification pass. Next: user-only named pipes/Unix sockets, startup ownership and process tests, durable applyConfig, and executable/frontend integration. No OS endpoint or hardware acceptance gate is closed by duplex-stream tests. |
| 2026-10-05 | Added user-scoped local endpoints and OS ownership locks, with private-storage validation, bounded connection retries, accepted-stream ownership retention, and separate-process crash/IPC fixtures. | 120 local hub tests plus process fixtures, Clippy, Rust 1.89.0, package verification, and contract freshness pass. Windows checks include anonymous denial and permissive ACL rejection. Unix permission/link/cleanup checks await portable CI. Next: executable host startup/attach and shutdown integration, durable apply, protected credentials, and frontend publication. Milestones 2–5 remain open. |
| 2026-10-05 | Integrated the private host into regain-alpaca, bounded admission/readiness, and cancellation-safe supervisor cleanup. Fixed repeated actor cleanup and Windows pipe exhaustion exposed by host tests. | 124 hub tests plus process fixtures and 14 Alpaca tests pass locally, including three production-executable tests. Clippy, Rust 1.89.0, transport/core/hub/Alpaca packaging and existing standalone camera/HTTP simulation checks pass. Endpoint CI verifies Linux x64/ARM64 and macOS ARM64. Next: durable apply, protected credentials, capabilities, frontend IPC attachment and Alpaca publication; all remaining original gates stay open. |
| 2026-10-05 | Added supervised configuration apply through IPC, staged compare-and-swap persistence, atomic connection quiescence, client rebinding, and explicit post-commit blocked/warning outcomes. Addressed macOS peer-admission failure from the previous checkpoint. | 132 Windows hub tests plus process fixtures and 14 Alpaca tests pass locally, including file/constructor failures, competing editors, retained operations, cancellation/deadline uncertainty, panic recovery, and production-executable apply/restart. Clippy, Rust 1.89.0, package verification, and contract freshness pass. Portable CI remains required for this checkpoint. Next: protected credentials, capabilities, frontend attachment/publication, and all remaining original gates. |
| 2026-10-05 | Added user-scoped credential storage, shared write-only descriptors/IPC, immutable rotation through configuration apply, and deletion protected by the update gate. Reused private file checks; moved credential resolution off the async executor. | 141 Windows hub tests plus process fixtures and 14 Alpaca tests pass, including DPAPI/ACL checks, corruption/isolation, cancelled apply versus deletion, and production-host authenticated polling before/after restart. Clippy, Rust 1.89.0, contract freshness, and package verification pass. The previous checkpoint now passes all four portable CI platforms; new storage checks await CI. Next: capabilities and virtual/simulated sources, frontend attachment/publication, and remaining milestones 2–5. |
| 2026-10-05 | Added bounded setup inspection through shared source leases, Switch pagination and range validation, weather/safety probes, native property reuse, generation-fenced reads, shared request metadata, and IPC retry-delay reporting. | 152 Windows hub tests plus process fixtures and 14 Alpaca tests pass, including faults/cancellation/deadlines, apply exclusion, real managed Alpaca connection sharing, IPC, and seven simulated production workers. Clippy, Rust 1.89.0, contract freshness, and package verification pass. Credential storage passed all four portable CI platforms; setup inspection still awaits CI. Next: virtual/simulated sources, frontend attachment/publication, broader capability contracts, and all remaining original milestones. |
| 2026-10-05 | Added explicit simulated Switch, SafetyMonitor, and Weather sources, typed shared test controls, visible simulation status, and a runnable observatory example. Fixed the safety consumer startup generation race found by the new tests. | 160 Windows hub tests plus process fixtures and 15 Alpaca tests pass, including shared values, atomic validation, uncertain writes, stale/absent sensors, safety failures, and production-host restart. Clippy, Rust 1.89.0, schema freshness, and fresh package verification pass. Next: virtual sources and frontend attachment/publication; original milestones 2–5 and final acceptance remain open. |
| 2026-10-05 | Added local virtual sources with shared typed operation dispatch, transitive simulation marking, retained scalar ages, and safety evidence timestamps. Fixed cached safe evidence seeding newly connected policies. | 169 hub tests plus process fixtures and 16 Alpaca tests pass, including nested commands/uncertainty, age/confirmation/expiry, cycle rejection, cancellation and reference cleanup, and production-host apply after internal lease drain. Clippy, Rust 1.89.0, contract freshness, and package verification pass. Next: frontend attachment and Alpaca publication; original milestones 2–5 and final acceptance remain open. |
| 2026-10-05 | Added the bounded Rust frontend IPC client and shared-host attachment helper. Reviewed cancellation, uncertain writes, protocol limits, explicit reattachment, and launcher lifetime; corrected inherited Windows capture handles. | 177 Windows hub tests plus the endpoint fixture and 18 Alpaca tests pass, along with Clippy, Rust 1.89.0, schema freshness, and fresh package verification. Next: actual Alpaca/native frontend adoption, setup, and remaining original milestones. No frontend or hardware gate is closed by the client library alone. |
| 2026-10-05 | Connected the first three hub output classes to ordinary Alpaca HTTP mode, with dynamic discovery, separate client leases, scalar/error mapping, negotiated weather metadata, and shared-host lifetime. Review bounded connection tasks before spawning and preserved uncertain outcomes. | Five router/private-endpoint tests and one production HTTP process test added. Prior full suite plus targeted changes pass; final validation recorded in hub-review.md. Shared setup, modern interfaces, conformance, native frontends, and all remaining original milestones stay open. |
| 2026-10-05 | Added the first shared web editor, setup API, stable reference pickers, capability-aware choices, review/apply workflow, and explicit paged inspection. Corrected empty-string presence validation and LAN UUID generation during review. | 177 Windows hub tests plus the endpoint fixture and 26 Alpaca tests pass. JavaScript draft/contract checks, four Python schema tests, two .NET reader tests and net48 build pass. Chrome verified edit/review/apply and supplied a simulation screenshot. Complete setup refinements, modern interfaces, native frontends, and all original remaining gates before merge. |
