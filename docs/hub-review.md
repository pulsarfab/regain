# Hub review record

This records local review and tests for the single hub PR. Passing a foundation
test does not imply that a frontend, transport, or hardware gate has passed.

## 2026-10-06: cached output diagnostic API

Reviewed controller ownership, saved-revision fencing, pagination, sample epochs,
expiry/recovery, write uncertainty, weather averaging and exportable field scope.
The negotiated `outputStatus` operation uses the existing host and protected setup
endpoint without constructing output sessions, capability requests or source leases.
Pages are bounded to 32; an empty terminal page is explicit and invalid cursors
are rejected. Safety's whole-output decision includes members outside the page.

Corrections made during implementation/review:

- Reuse Switch's actual scalar age/range interpretation instead of a second
  diagnostic freshness rule. Expose configured write intent without implying
  runtime CanWrite or probing its capabilities.
- Keep an inactive safety controller unknown/unsafe even if another client has
  cached safe input. Preserve live raw/effective state, independent counters,
  reason/hold/age and policy. Reads cannot finish recovery; expired evidence can
  withdraw permission without waiting for another poll.
- Read weather on a private engine copy so diagnostics cannot modify live
  history or last-valid clocks. Project only selected scalar keys and relevant
  histories, retaining wind-speed dependencies for wind direction. Large unrelated
  vendor text stays outside the copy and reply.
- Include decision/sample generation and revision identities rather than
  implying atomic consistency between an engine cache and current source health.
  Retain uncertain writes without another command or reset.
- The initial pagination test accidentally duplicated an Alpaca source identity.
  Production validation correctly rejected it; the private mock fixture now uses
  a distinct device number and tests the real whole-output aggregation.

Verification: full `cargo test -p regain-hub -p regain-alpaca --locked` passes,
including 41 runtime tests and 13 HTTP publication/setup tests. Ten new tests
cover diagnostic invariants, recovery/expiry, inactive cached safe, whole-output
aggregation, sparse/reserved slots, weather fallback and independent sensor
failures, private history/clock preservation, uncertain writes and IPC/HTTP guards.
Strict Clippy, Rust 1.89.0 all-target checks, generated contract freshness, Node
contract suites and four independent schema checks pass. All 155 NINA regressions
pass. Actual net48 x86/x64 production-host fixtures now call this negotiated API
before connection and with two sibling leases; both pass with zero build warnings.
Logs: `artifacts/hub-output-diagnostics-{rust,clippy,msrv,nina,net48}.log`.

CI evidence from earlier increments: CLI creation eeb9208 passes both complete
runs 37466565218/37466555917. Native creation 3dd86b4 PR run 37468454801 is still
active, with seven completed jobs passing and Windows at installer checks. Its
push run 37468447863 fails the first x86 COM import fixture's five-second response
wait. NINA 155 and both net48 fixtures passed first. Full failure output is retained
at `artifacts/hub-native-creation-push-ci-failure.log`; do not infer a cause or
consider this checkpoint fully accepted before investigation.

Next required diagnostic work: native/web presentation and protected exports
using shared descriptors, and actual source actor retry scheduling. The API is
not evidence of rendered diagnostics, interactive acceptance or any remaining
typed-device/camera/coordination/conformance/hardware milestone. All original
gates remain required before merging the single PR.

## 2026-10-05: contracts and safety/configuration foundation

Reviewed against main `c8fd7c4`, the original hub plan, the installed ASCOM/NINA
interface packages, and the pinned Field Kit safety source and license.

Review covered source/worker ownership boundaries, canonical source identity,
stable output/channel IDs, update revision conflicts, persistence failure,
credential redaction, graph cycles, observation ordering, freshness, aggregate
restoration, shutdown, and per-consumer confirmation cadence.

Findings fixed:

1. An active-only identity map allowed deleted numbers to be reused. Persist an
   append-only assignment ledger and reject edits to it through apply. Tests
   delete, restart, restore the original identity, and reject a different one.
2. A watch subscriber could outlive its safety publisher and keep the last safe
   result. Drop now publishes unsafe. The expiry task and shutdown publication
   share a lock so the task cannot overwrite that final result with safe.
3. Counting every shared sample could accelerate confirmation. Each membership
   enforces its own cadence; unsafe/failure observations still clear recovery
   progress immediately. Tests compare independent policies on the same stream.
4. Completing the recovery hold in a getter could authorize safety without a new
   observation. Permission and recovery confirmation are established only by
   eligible observations. Late safe results cannot erase an earlier expiry.
5. A removed source, wrong type, dependency cycle, or stale config revision could
   misroute or ambiguously apply an edit. Validation and serialized compare/apply
   now reject these; disk errors before replacement leave the live state intact.

Local verification:

- `cargo test -p regain-hub --locked`: 17 policy/runtime/metadata unit tests and
  10 configuration integration tests passed.
- `cargo clippy -p regain-hub --all-targets --locked -- -D warnings`: passed.
- `cargo fmt --all` applied; final diff/format checks run before commit.
- `cargo +1.89.0 check -p regain-hub --all-targets --locked`: passed.
- `cargo package -p regain-hub --allow-dirty --locked`: passed; packaged crate
  independently compiles and includes the JSON example fixtures and license.

Pending review gates: complete source/output descriptor coverage and generated
frontend contracts, source sampling/ownership and IPC, Windows COM isolation,
native NINA/ASCOM outputs, protocol conformance, camera images/coordination,
real-device acceptance, and user-facing documentation/screenshots. These remain
required by the plan and are not covered by the current 27 tests.

## 2026-10-05: shared configuration description and frontend contracts

The complete source/output structure now derives JSON Schema from its Rust
types. Existing policy schemas plug into that derivation, retaining the one
declaration for defaults, bounds, labels, descriptions, groups, and units.
The native .NET reader (shared by NINA and ASCOM) and JavaScript reader interpret
the same document. Tagged variants provide conditional fields without evaluating
arbitrary expressions. Runtime capability names gate choices/fields, while the
engine remains authoritative for semantic validation and applying changes.

Review corrections: align Unicode label lengths with JSON Schema's character
counts; preserve hidden identity history rather than rebuilding configuration
from visible fields; lock stable numbers after creation; keep unsupported
capabilities disabled instead of treating missing information as support.

Verification: 28 Rust tests, 4 independent Draft 2020-12 validation tests,
JavaScript contract tests, 2 native .NET reader tests, net48 build, Clippy with
warnings denied, Rust 1.89.0 check, and generated-file freshness check passed.
The tests cover defaults, units, types, conditional fields, capability gates,
immutable identities, unknown contracts/references, malformed documents and
structural-versus-semantic validation. CI runs the contract freshness check and
independent schema/web tests; native tests are part of the existing NINA suite.

This closes descriptor coverage at the library/reader layer. Actual setup windows,
runtime capabilities, host IPC, source polling, and frontend acceptance remain
milestones 2–4; the reader tests do not substitute for those gates.

## 2026-10-05: shared source actors, Alpaca adapter, and safety binding

Reviewed ownership and cancellation at the source boundary. One bounded actor
serializes each source's I/O while cached snapshots remain independently readable.
An immutable registry validates the complete graph before constructing adapters.
Clients share connection leases but require exclusive control for writes. The
safety output owns membership policy and leases; it does not duplicate polling.

Findings fixed:

1. Acquiring another lease could force an early poll, bypassing an upstream
   Retry-After. Additional clients now reuse the existing schedule, and initial
   connection failure also respects that delay. Unrepresentable deadlines suspend
   automatic polling rather than shortening the server's requested delay.
2. Timed-out reads could reuse a desynchronized transport. Reset retires its
   generation, clears cached values, and invalidates safety before another poll.
   Plain HTTP errors retain the generation so bounded communication grace works.
3. A timed-out write could be replayed after reconnect or control transfer. The
   actor latches uncertainty across both; an acknowledged rejection preserves its
   upstream code and remains distinct from an ambiguous operation.
4. A cancelled connection acquisition could leak a lease. Failed reply delivery
   removes only the newly acquired lease; a cancelled repeated control claim
   preserves the client's previous ownership.
5. A lagging subscriber could miss unsafe and consume only a later safe tail.
   The safety binding invalidates evidence, discards that tail, and waits for new
   observations. A deterministic overflow test exercises this exact sequence.
6. JSON map parsing would silently accept the last of duplicate `Value` fields.
   Typed envelope parsing rejects duplicates and malformed present fields. The
   explicit Field Kit compatibility allowance for omitted ErrorNumber remains;
   a safety sample still requires a JSON Boolean. Arbitrary response/error text
   never enters diagnostics.
7. A source owner could accidentally disconnect an externally managed device.
   Cleanup only writes disconnect after this adapter acknowledged opening the
   connection. An ambiguous connection write is not retried or claimed as owned.
8. Per-response limits alone did not bound a multi-sample text cache. Both the
   streamed response and aggregate scalar text storage now have size limits.

Verification:

- 50 Rust tests: 19 unit tests, 10 configuration tests, 13 actor/safety integration
  tests using virtual time, and 8 tests against a real loopback HTTP server.
- Tests cover two clients/outputs sharing sources, independent recovery policies,
  offline recovery, stalled unrelated sources, queue overload, cancelled requests,
  final-lease cleanup, uncertain writes, retries/exhausted cycles, long and dated
  Retry-After, streamed oversized replies, malformed/duplicate JSON, non-Boolean
  safety, no redirect/retry, HTTP-to-safety transitions, and independent expiry.
- `cargo clippy -p regain-hub --all-targets --locked -- -D warnings`, Rust 1.89.0
  compatibility, standalone crate packaging, formatting/diff checks, and the
  generated configuration freshness check passed.

Remaining gates are unchanged: typed switch/weather behavior and partial sensor
failures, capability discovery and modern connection negotiation, native worker
adapters, protected credential resolution, process ownership/IPC and resume,
Alpaca setup/publication, COM imports, NINA/ASCOM outputs, broader proxies and
camera coordination, conformance, hardware trials, and user documentation. These
tests use simulated devices and a local HTTP fixture, not attached hardware.

## 2026-10-05: typed switch/weather controllers and scalar sample status

Reviewed against the ASCOM canonical [Switch](https://ascom-standards.org/newdocs/switch.html)
and [ObservingConditions](https://ascom-standards.org/newdocs/observingconditions.html)
interfaces. These are library controllers; frontend conformance is still pending.

Corrections and refinements:

1. Stable channel IDs cannot be represented by compacting an active channel list.
   MaxSwitch now includes retired slots; removed slots are unavailable and never
   target a different device. Active and historical slot numbers are bounded.
2. An output's writable flag cannot confer source capability. Writes check
   CanWrite and live limits/steps, round to the exposed step, and hold a unique
   operation lease. Scalar properties remain read-only. Generation checks occur
   inside the actor immediately before dispatch, not only in the caller.
3. Cancelling a write must release its control/connection lease without replaying
   it. Cancellation tests exposed that reconnect could hide uncertainty behind a
   metadata error. The uncertainty latch is now explicit in source status and
   survives successful reads/reconnects while any session remains connected.
4. Successful writes invalidate cached state and schedule a confirmation poll;
   they never publish an optimistic value or report pre-command state as current.
5. A missing sensor previously failed a whole multi-property poll. Sample batches
   now carry per-property errors/ages. An HTTP test verifies one failed pressure
   sensor alongside a usable temperature and two shared weather clients.
6. Treating every HTTP read as a new sensor update would rejuvenate stale weather.
   ObservingConditions samples query sensor age before the value, reject unknown
   ages, and add local elapsed time. Expiry/fallback preserve last-update age.
7. Averaging incompatible units or two fallback sensors would fabricate a result.
   Unit validation, history reset on source/generation changes, time weighting,
   and circular wind averaging now have tests. WindGust retains its upstream peak
   statistic. Humidity/dew point pairing and one output-wide averaging period are
   validated before configuration apply. Boolean/nonfinite weather readings fail.
8. Multiplying a large finite sample by its duration could overflow before division.
   Normalize weights first, and reject any nonfinite result. Tests include large
   finite values and confirm repeated getters do not grow history.

Verification: 63 Rust tests (19 unit, 10 config, 13 source/safety, 9 HTTP,
6 switch, 6 weather), Clippy with warnings denied, Rust 1.89 compatibility,
generated schema freshness, 4 independent schema tests, JavaScript reader tests,
and 2 native .NET reader tests. The mixed-weather example joins executable
configuration fixtures. All device I/O here uses simulation or loopback HTTP.

Outstanding review work: whole-batch polling budgets need latency/large-source
testing before frontend exposure; capability/connection negotiation, Refresh,
native worker transport, protected credential resolution, host IPC/resume,
frontend error mappings, conformance and hardware acceptance remain open.

## 2026-10-05: incremental polling and weather refresh

1. One deadline covering a whole weather batch rejected healthy individual
   requests. Alpaca sample steps now issue one request each; sensor age/value
   requests are separate, with a conservative shared age anchor. Commands can
   run between steps without waiting for all channels.
2. A single batch timestamp/sequence would rejuvenate cached fields and multiply
   averaging observations as other fields arrived. Cache entries now retain their
   own timestamps and sequences. Transport resets clear both; writes invalidate
   samples and restart confirmation polling.
3. Incremental updates could evade the previous aggregate response limit. The
   actor validates scalar batches and bounds the combined text cache, key count,
   and historical sequence keys before publishing changes.
4. Transient per-field failures must retry that field and retain Retry-After.
   Exhaustion advances the poll plan; manual Refresh cannot bypass the delay.
   Safety remains one Boolean IsSafe observation per attempt, preserving its
   existing failed-cycle and recovery semantics.
5. An initial Refresh draft waited for a complete pass. The canonical
   [ObservingConditions interface](https://ascom-standards.org/newdocs/observingconditions.html#ASCOM.ObservingConditions.Refresh)
   requires a short trigger instead. Weather Refresh now invokes the upstream
   trigger, schedules local polling, and returns without waiting for sensor data.
   Multiple sources trigger concurrently; cached ages remain unchanged until
   actual readings arrive. Trigger failures are returned without replay.

Verification: 70 Rust tests (19 unit, 10 config, 16 source/safety, 13 HTTP,
6 switch, 6 weather), Clippy with warnings denied, Rust 1.89.0 compatibility,
standalone packaging, formatting/diff checks, and schema freshness passed.
New tests cover a polling pass longer than the request deadline, intervening
commands, unchanged earlier sample evidence, same-key retry, Retry-After during
partial polling and Refresh, a hung sensor after Refresh, aggregate text limits,
and rotating keys. Fixtures use virtual time or loopback HTTP, not hardware.

Next: shared native accessory worker transport and hub adapters, followed by
capability negotiation and host IPC. Frontend conformance, hardware acceptance,
and all later milestones remain required; no milestone gate closes here.

## 2026-10-05: common native accessory transport

Moved the existing Alpaca accessory worker client into `regain-core::accessory`
so the hub can use the same process transport without depending on HTTP frontend
code. Existing CAA/Falcon, EFW/EAF/FC3/ETA, and OFP2 endpoints now use it.

Review corrections:

1. The old client bounded elapsed time but not response memory. Both directions
   now enforce framing limits (4096-byte request, 1 MiB response); partial EOF,
   malformed replies, duplicate envelope fields, and oversized data fail.
2. Cancelling an awaited request could leave its delayed response in the stream.
   A request guard now closes stdin and retires the child on cancellation,
   deadline, or invalid framing. A subsequent request reports disconnected.
3. Workers now use the existing Windows kill-on-close job ownership. Ordinary
   close still offers the device worker its EOF shutdown policy before a bounded
   forced termination. No operation is retried by this client.
4. An `ok:false` reply only proves that a worker answered; the underlying USB
   command may already have been sent. `CommandFailed` remains distinct from
   framing/transport failure but must be treated conservatively for writes by
   the forthcoming native hub adapter. It is not a proof of pre-dispatch rejection.
5. Package verification resolved published regain-core 0.5.10, which lacks the
   new API, even when packaging the workspace. Development now uses 0.6.0 and
   Windows 0.6.0.0; maintenance remains on release/0.5. Nothing was published.

Local verification: 18 core unit tests plus a self-hosted process fixture covering
valid replies, command errors, local rejection, graceful EOF, malformed/oversized
responses, partial EOF, deadlines, and cancellation. Clippy and Rust 1.89.0 checks
pass for core and Alpaca. Seven simulation suites pass: EFW/EAF, CAA, Falcon,
FocusCube3, ETA, OFP2, and dynamic focuser slots. These use the production workers
with simulation enabled and exercise client sharing, motion/settings, calibration,
coordinate persistence, and error paths. Native hub source mapping is next.
After the version correction, `cargo package --workspace --allow-dirty --locked`
verified all ten package archives against the unpublished workspace dependencies;
`cargo +1.89.0 check --workspace --all-targets --locked` also passed.

## 2026-10-05: native accessory sources and mixed-source controller integration

Implemented a single native adapter over the common worker client for CAA, EFW,
EAF, FocusCube3, Falcon, OFP2, and ETA. Hardware protocol and motion/calibration
coordination stay in their existing workers. Reviewed against those workers and
the current Alpaca property/command mappings.

Corrections and deliberate boundaries:

1. Connect verifies the selected identity. Failed identity requests must retire
   their worker slot; otherwise a second connect could mistake a partially opened
   session for a verified connection. No implicit selection or simulation fallback
   is permitted. Simulation is explicit in returned identity for every vendor.
2. Missing temperature is a per-field error; position/motion remains usable.
   Whole-device fault/error fields stop publication of normal telemetry. Cached
   values and freshness still belong to the shared source actor.
3. Movement and brightness parameters are validated before dispatch. Focuser and
   wheel targets also use current hardware limits. Worker failures after a write
   map to uncertain, including framed worker errors; arbitrary worker exception
   text never enters hub diagnostics. Transport loss retires the generation.
4. ETA and EFW have no supported hardware halt command. Rotator sync/reference
   and related settings require persistent coordinate state before enablement;
   camera backends require Session rather than this scalar accessory adapter.
   These remain explicit later gates, not silently approximated capabilities.
5. Worker requests use the configured deadline, rather than the legacy client's
   fixed default. Windows local/CI test orchestration supplies the built worker
   directory so native integration tests do not silently skip there.

Verification: 79 Rust hub tests (21 unit, 14 HTTP/mixed-source, 10 configuration,
6 native-worker integration, 16 source/safety, 6 switch, 6 weather) pass locally
with `REGAIN_TEST_WORKERS` selecting the built production workers in simulation.
The native matrix checks all seven families, short moves, local validation,
identity mismatch, no fallback, illumination including zero, full cover motion
and halt, EFW calibration/provisional slot count, shared leases, and independent
client disconnect. A mixed source test writes a remote Alpaca switch while two
Switch clients and one Weather client share native FocusCube3 temperature.
Clippy, Rust 1.89.0, schema freshness, and formatting/diff checks pass.

The host factory, capability/connection negotiation, protected credentials,
cross-process ownership/IPC/resume, actual frontend devices, and all later
conformance/hardware/documentation gates remain open. No hardware was moved by
these tests, and this checkpoint does not close milestone 2.

## 2026-10-05: bounded Alpaca connection negotiation

Reviewed incremental connection state transitions against modern ASCOM interface
declarations and the existing source actor. InterfaceVersion now selects modern
Connect/Disconnect/Connecting or the legacy Connected property. Shared snapshots
expose the negotiated method, version, ownership, and connection uncertainty.

Corrections from review:

1. Already-connected modern hardware may belong to another client. Managed mode
   explicitly claims its own ClientID; externally managed mode sends no connection
   writes. A legacy connection already open remains borrowed.
2. Per-request timeouts alone cannot bound a driver reporting Connecting forever.
   A separate schema-described connection deadline bounds all handshake steps,
   including slow metadata. Pending steps admit queued commands and do not count
   as failed safety polls or successful recovery observations.
3. Disconnect previously could be retried after an ambiguous reply. Mark the
   cleanup attempt before awaiting it and retain uncertainty across resets.
   Cancelled Connect is likewise never retried or claimed for cleanup without
   an acknowledgement. Reconnect waits for a preceding asynchronous Disconnect.
4. A handshake failure was reset once by connect and again by poll/read/write.
   Only operations actually dispatched after connection now perform their own
   transport reset. The pending-adapter fixture verifies one reset at expiry.
5. Legacy metadata fallback is limited to explicit absence. HTTP 404 is distinct
   from an ASCOM ErrorNumber of 404. Unsupported modern Connect does not authorize
   a second connection write through the legacy property.

Verification: 89 hub tests pass (21 unit, 23 HTTP/mixed-source, 10 configuration,
6 native-worker, 17 source/safety, 6 switch, 6 weather). New cases exercise modern
and legacy ownership, cancellation, stalled/slow handshakes, uncertain cleanup,
reconnect during disconnect, and source polling after an incremental handshake.
Clippy, Rust 1.89.0, package verification for transport/core/hub, schema freshness,
web and independent JSON Schema readers, and both .NET HubConfiguration tests
pass. Production worker tests remain simulation only. Device-specific capability
discovery, host construction/IPC, protected credentials, frontend conformance,
hardware acceptance, and all later gates remain open.

## 2026-10-05: configuration-driven source construction

The source factory now derives a single polling plan from every output mapping
and prepares the existing native/Alpaca adapters before starting the registry.
The mixed native/HTTP controller test uses this path instead of hand-written
backend selection. Reviewed construction order, cadence, sample limits, and
credential handling.

1. Shared output mappings must not multiply polls or safe confirmation counts.
   Readouts are deduplicated by canonical sample key, while every SafetyMonitor
   plan contains exactly one strict IsSafe observation per attempt. Unrelated
   properties on a SafetyMonitor are rejected instead of mixed into safety polls.
2. A weather property used as a Switch gauge still needs its upstream sensor age.
   Age requests follow source type, not the frontend consuming the reading.
3. Enabled safety memberships select the fastest required shared cadence.
   Disabled memberships cannot speed up polling. Each output retains its own
   confirmation interval, and the persisted configuration is not rewritten.
4. Individually valid outputs can jointly exceed a source's sample limit. The
   limit applies to the union, and insertion stops at the first excess sample
   rather than allocating an unbounded temporary plan before rejecting it.
5. Invalid configuration is rejected before credential resolution. Adapters are
   prepared without hardware/network I/O, and credentials are resolved once per
   source. Missing providers fail closed; protected OS storage is not yet present.
   Actual HTTP requests carry the resolved header while source diagnostics omit
   both the secret and the reference. No backend failure enables simulation.

Verification: 95 hub tests (including five factory tests and one authenticated
loopback integration) pass with production accessory workers in simulation.
Clippy, Rust 1.89.0, generated-contract freshness, and package checks pass.
The runtime still needs output/session ownership, virtual and simulated sources,
protected credential storage, IPC, and actual frontend publication. COM/native
camera adapters and the remaining original milestone gates also remain open.

## 2026-10-05: shared output sessions and explicit shutdown

Added the per-revision output runtime for Switch, SafetyMonitor, and Weather,
with host-generated client identities and independent connection maps. The
mixed native/HTTP test now builds this runtime and connects three clients.
Reviewed reservation ownership, lock scope, policy lifetime, and source teardown.

1. Creating a safety policy per client would restart recovery and make clients
   disagree. One active policy is shared per output. A weak cache releases it
   after the last output guard, immediately invalidating retained subscribers;
   reconnect starts without permission inherited from source caches.
2. Weather clients share averaging settings and history. Source leases remain
   independent across clients and output types, so removing Switch clients does
   not interrupt Weather's source or change its connection generation.
3. A pending connect cannot hold a global mutex during source I/O. Client maps
   reserve a token, release the lock, then await construction. Cancellation and
   EOF remove only that reservation; an old task cannot delete its replacement.
   Duplicate ready connects are idempotent and duplicate pending ones are busy.
4. Disconnect while a write is in flight is not rollback. The command's guard
   retains its activity and source leases until its bounded result, including
   an uncertain outcome. Another client's connection is unaffected.
5. Dropping client references is insufficient evidence that workers have closed.
   Runtime shutdown closes admission and revokes safety synchronously before
   draining actors. Actors finish bounded I/O, reject queued/future commands,
   clear caches, and retain terminal cleanup results. All source cleanup is
   awaited even after a failure. A cancelled shutdown can resume without a new
   Disconnect attempt; runtime admission remains closed.

Verification: 104 hub tests pass, including eight runtime lifecycle/fault cases
and a queued-command/source-shutdown case. The latter verifies that shutdown
does not dispatch a queued read after an uncertain in-flight write. Runtime
tests also cover retained safety subscribers, independent expiry during a source
stall, cancelled connection replacement, an uncertain disconnect, resumed drain,
and stale-registry rejection. The mixed runtime test uses a real loopback Alpaca
server and production FocusCube3 worker in simulation. Clippy, Rust 1.89.0,
generated-contract freshness, and package verification pass.

This is still an in-process runtime. IPC framing/endpoints, OS ownership/resume,
configuration replacement, protected credentials, virtual/simulated source
adapters, frontend publication, conformance, and hardware acceptance remain open.
No milestone 2 completion or later milestone completion is claimed.

## 2026-10-05: bounded scalar IPC and typed dispatch

Added the versioned hello, bounded length-prefixed JSON transport, and controller
dispatch over a host-supplied stream. Each stream creates its own runtime client.
Configuration descriptions, local configuration reads, schema/relationship
validation, device/source status, and typed Switch/Safety/Weather operations use
the existing engine. Durable apply is not advertised by this dispatcher.

Review findings and corrections:

1. A source request must not block detection of EOF or another cached getter.
   A dedicated reader, bounded input buffer, and at most eight operation futures
   let cached safety results overtake stalled source capability reads. A slow
   reader still has a bounded response-write deadline and releases its leases.
2. Spawning requests in order does not prove their tasks start in order. Each
   operation is polled once before the next request is accepted. Disconnect thus
   sees a preceding Connect reservation, without waiting for its driver I/O.
3. Strict enum deserialization did not reject extra fields on no-argument unit
   variants. A malformed-message test exposed the gap. Empty struct variants now
   enforce unknown-field rejection, including nested get/put members. The test
   has its own deadline so a parser regression cannot leave the suite waiting.
4. Request IDs strictly increase; a repeated ID closes the stream without
   dispatching another write. Client identity fields are rejected. Unknown
   versions/commands and malformed typed values cannot reach a backend.
5. Frame reads and response serialization enforce the same 1 MiB bound. Escaped
   strings can make a valid source snapshot too large when encoded; a bounded
   serializer returns responseTooLarge while preserving framing and usability.
6. Put deadlines return uncertain rather than inviting retry. EOF/server-task
   cancellation closes only that stream's client, including pending connections.
   Validated configuration is not confused with successful runtime preparation
   or durable apply; validation explicitly reports its configuration-only scope.

Verification: 114 hub tests pass, including ten duplex-stream integration/fault
tests using the complete runtime and fault-injected sources. Tests exercise all
three output types, shared settings, configuration reads, unknown IDs/classes,
out-of-order responses, pending-connect cancellation, timed-out writes, request
replay, malformed/oversized/partial frames, overload, and a stalled reader.
Clippy, Rust 1.89.0, and package verification for transport/core/hub pass.

No listener is opened by this module. User-only OS endpoints, startup ownership,
separate-process tests, durable configuration replacement, protected credentials,
resume handling, executable integration, and all frontend/conformance/hardware
gates remain open. Camera buffers/images retain their separate planned contract.

## 2026-10-05: protected local endpoints and OS ownership

Added named pipes/Unix sockets keyed by canonical configuration path and OS user.
Binding consumes an OS-held exclusive lock on a separate persistent file. Atomic
config replacement cannot replace that lock, and process death releases it.

Review findings and corrections:

1. Listener lifetime alone is insufficient: accepted streams retain the ownership
   guard. The executable host must still retain ownership until runtime/source
   shutdown completes, including commands whose initiating client has disconnected.
2. Windows creation permissions do not prove existing storage or a connected
   server is private. Validate owner and protected user-only DACL on opened handles;
   reject reparse points and permissive ACLs without modifying them. Inspect ACE
   type/size before casting to an allowed ACE. Use identification-only client
   impersonation rights and reject remote named-pipe clients.
3. Unix directory/socket/lock modes and effective UID are checked; peer credentials
   verify both connection directions. Reject links and multi-link lock files. Only
   an actual owned socket can be removed as stale. Drop checks device/inode so it
   cannot remove an unrelated replacement at the same path.
4. Readiness retries are bounded and limited to absent/busy/refused endpoints.
   Permission errors do not enter the retry loop. A later launcher must validate
   hello rather than equating successful connect with readiness or compatibility.
5. Process fixtures use real child processes and OS locks: a competing process is
   denied before and after atomic config replacement; killing the owner permits a
   new owner. Another child serves real versioned IPC and drains on client EOF.

Windows endpoint/process tests pass, including anonymous access denial and an
explicit Everyone ACL fixture for directory/lock rejection. Unix mode, symlink,
hard-link, and replaced-socket tests are implemented but not locally executed:
the installed WSL distribution has no Rust toolchain. Portable CI must verify them.
No hardware is accessed by endpoint fixtures.

Full local verification: 120 hub tests plus the separate-process fixture pass.
Clippy with warnings denied, Rust 1.89.0 all-target checks, transport/core/hub
package verification, generated-contract freshness, formatting and diff checks
also pass. Existing production native-worker tests use explicit simulation.

Executable host startup/attach, global client admission, durable configuration
apply, protected credentials, resume, and all frontend/conformance/hardware gates
remain pending. These endpoints do not yet expose a user-facing hub service.

## 2026-10-05: shared executable host and bounded service lifetime

Added `regain-alpaca --hub-host --hub-config ABSOLUTE_PATH`. Ownership is acquired
before preparing sources; a losing invocation only verifies the existing owner's
bounded hello and exits. This mode opens no HTTP/discovery listener. It rejects
ambiguous normal-server/stdio options and invalid configs without echoing JSON.
Protected credential references remain unavailable until their provider lands.

Review findings and corrections:

1. Aborting a caller that awaits service shutdown must not abandon cleanup and
   release ownership. The supervisor starts immediately; dropping its waiter
   cancels admission but lets cleanup run independently, retaining the listener
   until all source actors finish. Its Tokio runtime must remain alive. Tests
   retain unsafe subscribers and force an uncertain disconnect during cancellation.
2. Closing safety leases could race actor Shutdown, causing two Disconnect calls.
   The actor now retains its last cleanup result until a new connection lifetime.
   Tests verify uncertainty survives shutdown without replay and that a later
   connection still receives its own cleanup.
3. A fixed Windows pipe-instance limit counts instances whose server handle closed
   while the former client retains its handle. This could prevent creation of the
   next listener and stop the host. Bound live service tasks at 32 instead; the
   listener remains replaceable. Admission tests retain malformed-client handles
   while successfully opening another client.
4. Readiness verifies reply correlation, protocol, instance UUID, runtime/client
   UUIDs, and supported frame/concurrency bounds under one overall deadline. A
   silent endpoint or wrong hub identity cannot masquerade as a ready owner.
5. Individual IPC failures are isolated. Shutdown stops admission, closes clients,
   drains source cleanup, and reports cleanup/listener failures without replay.

Local verification: 124 hub tests plus the process fixture and 14 Alpaca tests
pass. Three tests launch the production executable, proving duplicate-launch
handling, crash/restart, invalid arguments/config rejection, and real loopback
Alpaca safety changes delivered over protected IPC. Existing standalone HTTP
ImageBytes tests pass for SDK simulation and all four direct camera simulations.
Clippy, Rust 1.89.0 checks, and transport/core/hub/Alpaca package verification pass.
Endpoint commit `0c8bfe7` passed Linux x64/ARM64 and macOS Intel/ARM64 CI; the newly
integrated host still needs its own portable CI results.

Frontend automatic launch/attach, durable applyConfig, protected credentials,
capability discovery, resume handling, virtual/simulated source adapters, and
Alpaca/NINA/ASCOM publication remain pending. No milestone 2 or later gate is closed.

## 2026-10-05: supervised configuration apply and revisioned runtime publication

The persistent service now handles applyConfig and hostStatus through IPC and the
shared executable. A read-only embedded runtime advertises only its supported
operations. Configuration errors retain field paths; disk/backend details and
credential values are not exported as arbitrary exception text.

Review findings and corrections:

1. Checking connection counts without excluding a concurrent reservation races.
   Quiescence and connect reservations now share the lifecycle mutex. Pending
   connects and guards retained by in-flight commands count as active. The current
   whole-runtime replacement affects all outputs, so all must be disconnected.
2. Validation alone does not prove that adapters can be built. Stage the candidate
   file, prepare the next runtime without device I/O, verify it matches the entire
   candidate configuration, and recheck revision/store identity at atomic
   replacement. Rejecting/dropping prepared values removes
   their temporary files. Unused actors dispatch no Disconnect on retirement.
3. Device cleanup must not precede a fallible replacement that promises to preserve
   the old running configuration. After commit, drain the old runtime and only then
   enable the new one. Uncertain cleanup retains the new persisted revision but
   blocks device admission; this is an explicit applied/not-ready outcome.
4. RPC timeout, EOF, and caller cancellation must not split commit from activation.
   A supervised transaction retains the update gate until completion, and host
   shutdown waits for it. Unexpected task failure closes the old runtime and blocks
   admission. Clients inspect getConfig/hostStatus to resolve uncertain replies.
5. Stream client IDs and the host process ID stay stable through apply; bindings
   move to the new runtime on the next request. Retired runtime clients cannot
   reopen sources. All new safety policy starts without cached safe permission.
6. A flush failure after replacement is not rollback. Return a distinct committed
   result from the store and a persistence warning from the service. The saved file
   is flushed, as is its parent directory on Unix: file flush alone does not ensure
   directory-entry persistence ([Linux fsync documentation](https://man7.org/linux/man-pages/man2/fsync.2.html)).
   No hardware power-loss guarantee is inferred from these tests.
7. Configuration reads now enforce the size bound while reading, rather than
   relying on metadata that could become stale as a file grows. Staging happens
   outside the configuration snapshot mutex; filesystem work runs off the async
   executor and no service-state mutex is held during driver I/O.
8. macOS CI for `e283472` failed admission recovery after a queued client had
   disconnected. Unix accept now discards aborted connections and failed/mismatched
   peer credentials per connection. Listener errors still stop the service. The
   existing saturation/recovery test now reports the supervisor result on failure;
   the correction needs its portable CI result before claiming macOS recovery.

Local checks: 132 hub tests plus the endpoint process fixture, 14 Alpaca tests,
Clippy with warnings denied, and Rust 1.89.0 checks pass. Tests cover pre-commit
file/construction failures, competing editors, stale and foreign prepared updates,
pending connections, retained writes, cancelled/deadline-expired applies, blocked
cleanup, and an injected task panic. Production-executable tests apply a network
safety edit through IPC, reuse that stream, reject stale edits, reconnect, and
reload the committed revision after restart. A Unix permission fixture covers
post-rename directory-flush failure and awaits portable CI. Transport/core/hub/Alpaca
package verification and generated-contract freshness pass. Packaging used a fresh
target directory after Cargo's temporary registry reused an older archive with
the same unreleased 0.6.0 version.

Protected credentials, capabilities, automatic frontend attachment, OS resume,
virtual/simulated sources, publication/setup, COM/NINA/ASCOM integration, broader
proxies, coordination, conformance, hardware checks, docs/screenshots, and final
merge remain required by the original plan.

## 2026-10-05: user-scoped credentials and shared-host rotation

The shared executable now resolves credentials through user storage and exposes
write-only creation, status, and deletion over its existing private IPC. Metadata
includes common input keys/descriptions and the actual protection method.
Windows uses user DPAPI and protected user-only ACLs. Unix deliberately uses
private plaintext files (0600 under 0700), without requiring a desktop keyring.
This distinction is part of the frontend contract, not an encryption claim for Unix.

Review findings and corrections:

1. An in-place secret update would leave existing adapters using the old header
   while config still identified the same reference. References are immutable:
   create, revision-checked apply, then delete the unused old record. Existing
   adapters keep their resolved value until their runtime is replaced.
2. Deletion must not race configuration preparation or remove a credential still
   used by an old draining runtime. Mutations share the service update gate,
   inspect every configured source, and retain the gate in the blocking task even
   after cancellation. Shutdown also waits for it. A paused-builder test cancels
   the apply waiter and proves deletion remains busy, then becomes in-use.
3. Configuration files can be copied, and separate hosts can use the same reference
   text. Storage is scoped by canonical config path and OS user; record contents
   bind scope/reference, with matching DPAPI entropy on Windows. Copied records
   cannot resolve under another reference or scope. Moved configs need new records.
4. Private endpoint file checks are reused for storage. Windows checks the opened
   handle's owner/protected ACL and rejects reparse points. Unix checks owner,
   mode, regular-file type, and link count, uses O_NOFOLLOW, and opens nonblocking
   so an invalid FIFO cannot stall before type validation. Reads are bounded.
5. Secrets and raw frame buffers use clearing wrappers; HTTP headers are marked
   sensitive. Responses/errors contain no secret values. This does not promise
   complete erasure of third-party parser/HTTP or OS copies. No secret-read IPC
   operation exists. Invalid inputs fail before storage creation.
6. Disk and DPAPI operations run off the async executor, including adapter
   preparation during apply. Constructor errors/panics retain the existing
   transaction failure rules. A missing user storage location does not prevent
   credential-free hosting; authenticated sources fail closed without a provider.
7. Creation uses private staged files with no-clobber publication and flushes.
   A lost/failed creation response can leave a private orphan, never an in-place
   rotation. Deletion reports post-delete directory-flush uncertainty separately;
   neither operation promises forensic deletion or power-loss durability.

Local validation: 141 Windows hub tests plus the separate endpoint-process fixture
and 14 Alpaca tests pass. Added tests cover DPAPI round trips/wrong entropy,
anonymous denial and unprotected replacement rejection, immutable references,
corruption/oversize/scope/rebinding, IPC rotation/redaction, unavailable providers,
and cancellation during a competing apply. The production executable creates a
credential through IPC, authenticates a real loopback HTTP source, restarts and
authenticates again, then rejects in-use deletion and removes an unused record.
All values are explicit fake fixtures. Clippy with warnings denied, Rust 1.89.0,
generated-contract freshness, and transport/core/hub/Alpaca package verification
pass. Packaging used `target/hub-credentials-package` to avoid stale archives of
the same unreleased version. No config schema change was needed.

Prior checkpoint `d8ab064` now passes Linux x64/ARM64 and macOS Intel/ARM64 CI,
verifying the aborted-peer correction and Unix directory-flush fixture. Portable
CI is still required for the new credential permission/link checks. Device
capabilities, virtual/simulated sources, frontend attachment/publication, COM,
native NINA/ASCOM outputs, broader proxies, coordination, conformance, hardware,
docs/screenshots, and the final merge audit remain open.

## 2026-10-05: bounded setup inspection on shared sources

Added explicit inspectSource IPC for Switch/SafetyMonitor/ObservingConditions
network sources and native accessories, with shared request metadata. Inspection
uses the source actor and temporary lease rather than opening another transport.
It is an observation for setup; it does not authorize commands or seed safety.

Review findings and corrections:

1. A large switch bank cannot produce unlimited requests or metadata. Pages allow
   at most eight channels under one 20-second overall deadline, with the source's
   individual deadlines still enforced. Preserve upstream IDs and next-start;
   reject bad counts/pages. Bounded text and strict value types prevent malformed
   metadata from appearing as supported capabilities.
2. A failed read is not evidence of unsupported hardware. Keep observed,
   unsupported, and unavailable states distinct; only definitive NotImplemented
   maps to unsupported. Weather descriptions, ages, and values remain independent.
   Retry-After ends the scan without retry and reaches IPC as retryAfterSeconds.
3. An initial permissive range check differed from the write path. Inspection now
   reuses Switch Grid validation, including whole, representable step counts.
   Weather units and native property lists also use existing definitions. Native
   Boolean properties are not suggested as scalar mappings until those consumers
   support them. Do not infer Switch units from text.
4. Inspection reads can straddle a reconnect. Added an optional read generation
   fence at actor dispatch; the inspector also checks before/after every read and
   at completion. A changed connection invalidates the whole report. This does
   not promise an atomic snapshot of equipment values or across separate pages.
5. Temporary setup leases must count toward configuration quiescence, including
   pending connection attempts. Runtime activity is reserved under the same
   lifecycle mutex as apply/connect. Cancellation releases the temporary lease;
   already-dispatched reads retain their source deadline and registry drain rules.
6. Setup must preserve ownership. A real loopback modern Switch fixture runs two
   inspections while another lease remains, proves one Connect and no early
   Disconnect, then verifies exactly one owned cleanup. No motion or switch-write
   commands are dispatched by discovery. All native worker trials are explicit
   simulation and retain that fact in the result.

Local tests: 152 hub tests plus the endpoint process fixture and 14 Alpaca tests
pass. New cases cover Switch IDs/pagination/grid checks, strict safety values,
partial weather support, connection changes, Retry-After, cancellation versus
apply, the total deadline, fenced reads, private IPC dispatch/metadata, managed
HTTP sharing, and inspection of all seven simulated native accessory families.
Clippy with warnings denied, Rust 1.89.0, generated-contract freshness, and
transport/core/hub/Alpaca package verification pass. Packaging used the fresh
`target/hub-capabilities-package` registry. Credential checkpoint `c472922` now
passes Linux x64/ARM64 and macOS Intel/ARM64 CI, including private-file/link checks.
Setup inspection requires its own portable CI result.

Inspection of unconfigured devices, virtual/simulated source implementation,
frontend attachment/publication, complete proxy capabilities, COM/NINA/ASCOM
integration, coordination, conformance, hardware trials, documentation/screenshots,
and final merge remain open. This checkpoint does not close milestone 2.

## 2026-10-05: explicit scalar simulators and safety startup fencing

Implemented simulated Switch, SafetyMonitor, and ObservingConditions sources on
the shared source actor/runtime. Typed updateSimulation IPC and describeConfig
metadata supply common controls for future frontends; production executable tests
exercise their actual private endpoint. No physical device was used in this step.

Review findings and corrections:

1. A simulator must retain the production connection/control rules. Updates use
   temporary source leases and exclusive command control, participate in apply
   quiescence, and reject native sources before opening their transports. Patches
   validate a cloned state before replacement, preventing partial updates.
2. Injected uncertain writes change the simulated device once, then return an
   uncertain result. Clearing the injected fault leaves the actor latch intact;
   new commands remain blocked until every source lease is disconnected.
3. Simulation must remain visible and deliberate. Source/output diagnostics and
   capability inspection mark it, including native workers explicitly launched
   in simulation. No error path switches real hardware to a simulator.
4. Safety injection changes the raw source, not the policy result. Invalid types,
   read failures, and timeouts cannot count as safe; normal confirmation is still
   required. Restart/configuration replacement starts unsafe. Weather injection
   exercises actual age/fallback rules and represents missing sensors as errors.
5. Fast simulated connections exposed a real safety startup race. The policy was
   initialized from the construction generation, while its delayed consumer began
   from the latest generation and could reject every future observation. Carry
   the construction fence/sequence into the consumer so its normal transition
   handling synchronizes the policy. Regression tests cover the intervening
   connection and retain the earlier lost-unsafe-event/tail-discard test.

Local validation: 160 hub tests plus the endpoint process fixture and 15 Alpaca
tests pass. Seven simulator integration cases cover shared clients, safety
faults/recovery, stale/absent weather, atomic validation, read-only channels, and
rejection of real sources. The executable test verifies descriptor exposure,
updates, shared safety polling, and unsafe state after restart. The checked-in
simulated observatory example passes validation, identity, and round-trip checks.
Clippy with warnings denied, Rust 1.89.0 checks, and generated-contract freshness
pass. Transport/core/hub/Alpaca package verification passes using the fresh
`target/hub-simulation-package` registry. Setup inspection checkpoint 9ae966e now passes Linux x64/ARM64 and macOS
Intel/ARM64 CI; its Windows job is still running at this review.

Virtual sources, frontend attachment/publication, OS resume, COM imports,
NINA/ASCOM outputs, broader proxies and coordination, conformance/hardware checks,
documentation/screenshots, and final merge remain open under the original plan.


## 2026-10-05: local virtual sources

Implemented in-process Switch/SafetyMonitor/ObservingConditions composition,
bound once to the applied runtime through a weak reference. Internal clients use
the same output controllers and source actors as frontend clients. Extracted typed
scalar dispatch into shared handlers instead of duplicating IPC behavior.

Review findings and corrections:

1. Recursively recomputing simulation markers can revisit shared subgraphs
   exponentially. Propagate markers in bounded graph passes after cycle validation.
   Standalone factories reject unbound virtual sources before actors start.
2. A virtual read must not refresh stale evidence. Switch/Weather samples carry
   their existing ages through every layer. Weather metadata stays available when
   the reading is stale; Refresh reaches the actual source. Averaging remains an
   explicit per-output stage, including when multiple stages are chained.
3. Polling an already-safe inner policy must not manufacture confirmations or a
   longer lifetime. Propagate the oldest contributing safe request timestamp.
   Tests prove three confirmations cannot arise from one cached inner observation
   and a shorter outer safe deadline expires while the inner output is still safe.
4. A newly connected policy could accept old inner evidence on its first virtual
   poll. Capture a minimum evidence time at construction, generation transitions,
   and event loss. Ignore earlier safe evidence; do not seed new permission from
   a cache. Existing generation and lost-unsafe-event regressions remain passing.
5. Connected internal clients temporarily retain the runtime. Close them on reset,
   disconnect, and drop; retain the weak binding while idle. Tests prove complete
   reference release after normal disconnect, active-graph shutdown, and cancelled
   nested connection attempts against a stalled loopback source. Other direct
   clients keep their source connection when a nested client disconnects.
6. Preserve uncertainty and actual write permissions at every layer. A nested
   ambiguous write changes the simulated leaf once, blocks subsequent commands,
   and leaves another direct client's leaf uncertainty latch intact. No operation
   is automatically replayed to recover the virtual adapter.
7. Inner grace permission is not a new successful observation. A loopback HTTP
   failure test verifies the virtual source conveys read failure through the outer
   grace/expiry policy, clears recovery confirmation, and recovers only after fresh
   upstream evidence resumes.
8. Internal connections participate in apply quiescence. A production-executable
   test rejects an edit while connected, drains nested leases after disconnect,
   applies the edit, and uses the same IPC stream against the replacement graph.

Validation: 169 hub tests plus the endpoint process fixture and 16 Alpaca tests
pass locally, including nine new virtual-source cases and one new production-host
case. Clippy with warnings denied, Rust 1.89.0, generated-contract freshness, and
transport/core/hub/Alpaca package verification pass. The package check used the
fresh target/hub-virtual-final-package directory. Simulation checkpoint 6a607e5 passes
all four portable CI platforms; Windows remains in progress at this review.
This new checkpoint still requires its own CI results.

Frontend automatic attachment/reconnection, OS resume, unconfigured discovery,
Alpaca publication/setup, COM imports, native NINA/ASCOM outputs, broader device
proxies and camera/focuser coordination, conformance/hardware checks, documentation
and screenshots, and final audit/merge remain required. Milestone 2 is not complete.

## 2026-10-05: frontend client and shared-host attachment

Added a bounded Rust IPC client and an executable attachment helper. This is
shared infrastructure for the actual frontends, whose adoption remains pending.

Review findings and corrections:

1. Cancelled callers must not free a dispatched request's capacity or replay it.
   Retain permits through reply/deadline, skip unsent cancelled requests, and mark
   dispatched mutations uncertain on transport failure. A failed client is terminal.
2. A detached read task could retain leases after its client disappeared. Use an
   abort-on-drop task set and close both transport halves; a real runtime test
   verifies last-client drop releases the simulated source lease.
3. JSON value parsing hides duplicate envelope keys. Parse the envelope strictly,
   distinguish null results from absent results, reject unknown reply IDs, and
   retain structured remote errors and Retry-After without logging request bytes.
4. Bound encoding, negotiated concurrency, frame I/O, and total request lifetime.
   Keep cancelled-but-dispatched operations within the same limit. Reject invalid
   operations before writing and preserve zeroizing request buffers on failures.
5. The first Windows process test hung after its helper exited: the shared child
   inherited a capture pipe despite null standard handles. Use CreateProcessW
   with handle inheritance disabled and CREATE_NO_WINDOW. Production tests now
   prove the helper exits while its shared host survives, including paths with
   spaces and Unicode. Argument quoting has a separate Windows unit test.
6. A candidate PID cannot prove ownership. The attachment helper probes a held
   lock instead of replacing its owner, launches at most one candidate, validates
   hello, and never kills an owner on readiness failure or client disconnect.
   Test-only cleanup terminates only the newly launched empty fixture host.
7. Host loss requires explicit reattachment with a new host/client identity. Two
   clients share one host, disconnect independently, and cannot reuse the failed
   client after restart. Wait for the ownership lock to release before testing
   restart; closed client streams alone do not prove host cleanup has finished.

Validation: 177 Windows hub tests plus the endpoint process fixture and 18 Alpaca
tests pass. This adds seven client integration tests, one Windows quoting test,
and two production attachment tests. Clippy with warnings denied, Rust 1.89.0,
generated-contract freshness, and fresh transport/core/hub/Alpaca packaging pass
(`target/hub-client-package`). No test host processes remain. Unix launch behavior
still requires portable CI. Simulation checkpoint 6a607e5 has completed successful
push CI; virtual-source checkpoint 14abfc9 remains in progress at this review.

Alpaca HTTP/setup adoption, native .NET attachment, reconnection/resume behavior,
COM imports, broader proxies and coordination, conformance/hardware acceptance,
documentation/screenshots, and final audit/merge remain required.

## 2026-10-05: initial Alpaca HTTP outputs

Ordinary HTTP mode now attaches to the shared host and publishes dynamic Switch,
SafetyMonitor, and ObservingConditions outputs alongside existing equipment.
The initial mapping was checked against the
[Alpaca API](https://ascom-standards.org/api/) and
[Switch interface](https://www.ascom-standards.org/library/html/T_ASCOM_Common_DeviceInterfaces_ISwitchV2.htm).
It advertises the synchronous interface versions it implements. Modern interfaces,
complete protocol/error conformance, and shared setup are explicitly still required.

Review findings and corrections:

1. The server already has a generic accessory route. A second generic hub route
   conflicted at router construction. Route the three hub classes through the
   existing dispatcher, leaving existing native class handlers in place.
2. HTTP ClientIDs need independent private sessions, not one shared connection
   whose disconnect would revoke every client. Preserve configured UUIDs/numbers;
   cap sessions at 24, close the last-output session, and leave host capacity for
   native clients. Standard IDs remain correlation values, not authentication.
3. A per-client mutex alone would allow unlimited connection tasks to queue.
   Admit connection changes with a nonblocking gate before spawning. Keep accepted
   operations supervised through caller cancellation and retire the session after
   connection uncertainty/failure. No global mutex spans source I/O.
4. Hub Connected means a virtual-output lease. A stalled upstream must not turn
   into a valid cached reading or prevent another safety output from responding.
   The loopback upstream fixture verifies unavailable switch reads and independent
   safety while a managed upstream connection is stalled.
5. Weather properties need scalar values, while IPC retains age/provenance.
   Add SensorDescription through shared typed dispatch and advertise its capability
   so older hosts are not sent an unrecognized request. Missing sensors remain
   unsupported; average period and Refresh use the shared controller.
6. Forward uncertainty without raw driver text or retries. A simulated write
   changes its value once, returns uncertainty, and blocks another client's write
   after the injected fault clears. Neither HTTP nor the client resets that latch.
7. HTTP shutdown/process death must release only its own leases. Router tests
   retain a separate local client; the executable test kills its own HTTP child,
   verifies the same host instance survives, and observes its leases drain.
8. A successful write invalidates the old sample before the next poll. Correct
   the test to await a new valid reading, rather than assuming an immediate cache
   hit. Do not suppress the production unavailable error to satisfy the test.

Validation: the full local hub/Alpaca suite passed, followed by all five current
HTTP/private-endpoint tests and all eleven IPC tests after the capability addition.
Coverage now totals 177 Windows hub tests plus the endpoint fixture and 24 Alpaca
tests. The added production executable test uses a real loopback TCP listener.
Clippy with warnings denied, Rust 1.89.0, schema freshness, formatting/diff checks,
and fresh transport/core/hub/Alpaca package verification pass
(`target/hub-http-package`). No hardware was actuated. Virtual-source commit
14abfc9 passed both complete CI runs. Client/launcher commit 90037bb has passed
all four portable platforms; its Windows job is still running at this review.

Remaining: shared setup and configuration/reconnection UI, modern asynchronous
interfaces and conformance, native .NET clients/providers, COM imports, broader
proxies, camera/focuser coordination, OS resume, discovery, hardware acceptance,
documentation/screenshots, and final audit/merge. Milestone 2 remains open.

## 2026-10-05: shared web setup editor

The first `/setup/hub` editor uses the generated configuration schema and host
capabilities. It supports drafts, host validation, a redacted review, revision-
checked durable apply, cached source status, and explicit paged inspection.
The three published hub classes also expose their standard per-device setup URL.

Review findings and corrections:

1. Keep field keys, descriptions, units, bounds, defaults, references and choice
   gates in shared metadata. Add source/output reference hints and enum capability
   gates to the Rust schema; both JavaScript and .NET readers consume them.
   Unsupported camera/broader simulation/proxy choices remain visibly unavailable.
2. A required JSON property may contain an empty string. Browser validation
   incorrectly rejected empty unit labels. Require nonempty strings only when the
   schema supplies a positive `minLength`; leave host validation authoritative.
3. `crypto.randomUUID` is unavailable on ordinary LAN HTTP. Generate RFC 4122 v4
   identities with `crypto.getRandomValues`, preserve existing UUIDs and numbers,
   and keep saved identities read-only. Do not use a weak random fallback.
4. Serve a bounded, JSON-only, POST-only setup API with the existing same-origin
   check, exact media-type validation and no CORS grant. Whitelist configuration,
   status, inspection and simulation commands. Device connection/control and
   credential operations cannot be invoked through this initial setup API.
   This remains the server's existing trusted-network setup surface; the origin
   check is not remote-user authentication.
5. Edits invalidate the reviewed snapshot. Apply uses its loaded revision and
   never retries after a lost/uncertain reply. Reload saved revision and host
   status for reconciliation. Connected/stale configurations fail without writes;
   applied-but-blocked and persistence-warning outcomes remain visible.
6. Use shared inspection descriptors rather than duplicating bounds in the page.
   Inspection is an explicit temporary connection; ordinary status is cached.
   Preview redaction follows metadata and does not mutate the draft.
7. Chrome verified edit, review, apply, the new revision, and the updated output
   label. The screenshot in `docs/images/hub-setup-simulation.jpg` uses explicit
   simulation. No hardware was used. A blocked in-app browser dialog did not
   establish UI acceptance; the successful Chrome flow did.
8. PR CI exposed the existing response-size test's 50 ms deadline on macOS Intel:
   it timed out before receiving oversized headers. Give size validation its own
   3-second budget and the stalled mutation a separate 1-second deadline against
   a 5-second server delay. Retain permanent-size failure, uncertain-write and
   exactly-one-request assertions; production deadlines are unchanged.

Validation: the full local suite passes with 177 Windows hub tests plus the
endpoint process fixture and 26 Alpaca tests (`artifacts/hub-setup-tests.log`).
After the final test changes, all seven HTTP/setup integration tests and the
response-size/stalled-write test pass. JavaScript contract/draft checks, four
independent Python schema tests, two .NET 8 reader tests, a net48 build, Clippy
with warnings denied, Rust 1.89.0, generated-schema freshness, and fresh package
verification pass (`target/hub-setup-package`). HTTP checkpoint e27ad31 passed
complete push CI; its PR run failed only the timing-coupled test corrected here.
Disposable preview host/HTTP processes have been stopped.

Remaining: setup initialization, credential controls, simulation controls,
richer safety/cleanup diagnostics, reconnect/resume, modern interface conformance,
native NINA/ASCOM frontends, COM imports, broader proxies and coordination,
hardware acceptance, documentation/site updates, and the final audit/merge.
The setup/IPC 1 MiB frame limit is smaller than the store's 4 MiB file limit;
large-configuration transfer needs an explicit refinement. Milestone 2 stays open.

## 2026-10-05: modern scalar state and connection interfaces

The shared host now supplies cached DeviceState and supervised connection changes.
Alpaca publishes Switch v3, SafetyMonitor v3 and ObservingConditions v2 when their
capabilities are negotiated. This checkpoint adds the methods, not a conformance
certification or a completed frontend/hardware gate.

Review findings and corrections:

1. DeviceState must not perform source I/O or renew evidence. Clone each Switch
   source cache once and use the same sample for its boolean/numeric pair. Read
   Weather under one engine lock and time. Omit stale, failed, retired or absent
   readings independently, preserve real slot numbers, and use canonical ASCOM
   names. Safety evaluates its current shared policy rather than cached permission.
2. TimeStamp is optional measurement time. Mixed cached values have no single UTC
   measurement timestamp, so omit it rather than reporting query time as fresh
   evidence. Empty collections return []. Primary references are linked in the
   hub contract; the tests verify that repeated getters cannot defeat expiry.
3. An asynchronous acknowledgment must not leave an unbounded detached task.
   Reserve one operation per client before spawning, count it toward apply
   quiescence, impose an independent 30-second deadline, and retain failure.
   Overlapping legacy/modern changes return busy. EOF cancels pending reservations;
   a lost request waiter does not replay an admitted change.
4. The HTTP adapter must include pipe initialization in Connecting. Keep a bounded
   per-client supervisor, revoke every private lease on failed changes, and retain
   asynchronous errors until explicit connect/disconnect reconciliation. A polling
   caller must not mistake failure for Connecting=false. Reuse capacity after
   explicit disconnect and preserve other clients' leases.
5. Connected is a virtual-output lease, not a promise that every upstream is
   healthy. An initial failure fixture using an unavailable worker incorrectly
   expected lease acquisition to fail. Replace it with actual host admission
   exhaustion, await its bounded initialization deadline, then verify retained
   errors and recovery. No production health/connection semantics were changed.
6. Negotiate new capabilities before sending typed members to older hosts. Retain
   legacy versions there. Switch CanAsync=false is honest about scalar writes;
   async setters and StateChangeComplete are unsupported, while mandatory
   CancelAsync validates its channel and succeeds as a no-op. Local virtual-source
   reads use the same state/async contract. DriverVersion uses major.minor.
7. Review cancellation, queued operation admission, panic/drop guards, EOF before
   task start, and explicit failure clearing. There is no global lock across I/O,
   automatic replay, or cancellation claim about physical rollback. Successful
   lease disconnect does not claim all backend cleanup has completed.

Validation: 181 Windows hub tests plus the endpoint process fixture and 29 Alpaca
tests pass (`artifacts/hub-modern-tests.log`). Four shared-runtime/IPC and three
HTTP tests were added. They cover cached bundles without source reads, expiry,
slot tombstones, partial weather failures, canonical names, separate clients,
overlap, queued-task EOF, retained failure and reconciliation, and capacity reuse
across 26 client IDs. Clippy with warnings denied, Rust 1.89.0, generated-contract
freshness, formatting/diff checks and fresh transport/core/hub/Alpaca package
verification pass (`target/hub-modern-package`). Setup checkpoint f0962d9 passes
all four portable platforms, package and COM checks; Windows jobs are still
running at this review. No hardware was actuated.

Remaining: native .NET/NINA/ASCOM attachment and providers, complete scalar error
and conformance-tool checks, setup refinements, COM imports, broader proxies and
camera/focuser coordination, OS resume/recovery, hardware trials, documentation
and site updates, and final audit/merge. Original milestones 2–5 remain open.

## 2026-10-05: shared native frontend attachment and IPC

Added `Regain.Hub.HubAttachment` and `HubClient` to the existing shared frontend
assembly for .NET 8 and net48. These are transport/attachment APIs, not exported
NINA or ASCOM providers. Native setup and device adoption remain required.

Review findings and corrections:

1. Reuse the Rust attachment helper rather than introducing another source owner.
   Bound helper output/error capture and its lifetime, launch hidden without a
   worker job, and stop only the helper on cancellation. A candidate PID cannot
   authorize killing a shared host. Require fully qualified paths; rooted drive-
   relative paths such as C:config.json are insufficient.
2. Authenticate the opened pipe before hello. Verify owner, protected DACL and a
   single ordinary current-user allow ACE. Use identification rights and disable
   handle inheritance. Actual permissive-pipe rejection is tested before any
   handshake bytes are sent; the positive path uses the real Rust private endpoint.
3. Negotiate request/frame bounds, assign IDs under the writer gate, and reject
   unknown/repeated IDs, duplicate JSON keys and invalid envelopes. Null is a
   valid result, distinct from a missing result. Preserve structured remote fields
   without placing arbitrary response text in exceptions.
4. An advertised future operation cannot safely default to read-only. Refuse
   operations outside the known command set until their semantics are implemented.
   Bound outgoing tokens before encoding and clear temporary wire/helper buffers
   on success and failure. Configuration input retains credential references only.
5. Queued cancellation must free capacity without dispatch. Dispatched cancellation
   only ends the caller wait; retain its capacity and deadline until reply/loss.
   Frame/request deadlines close stalled transports even after caller cancellation.
   Dispatched writes fail uncertain and are never replayed.
6. An idle read task must not keep an abandoned public client alive forever. Pumps
   retain separate connection state; finalization closes the stream. Explicit
   Dispose and GC/EOF tests prove local release. Neither Closed nor Dispose claims
   physical rollback or completed server-side cleanup.
7. The first real-host test expected hostInstance in hostStatus, but that member
   belongs to hello. Correct the fixture to use negotiated identity and current
   service revision/phase; verify actual source lease counts after one client closes.
8. net48 x86 runtime passed, then x64 loaded a stale x86 dependency: SDK default
   intermediate paths do not distinguish PlatformTarget. Give each bitness a
   separate intermediate directory. Both fixtures now execute the public API,
   verify separate clients/leases and a surviving host, and clean up only their
   newly launched simulation-only test process after verifying its executable.

Validation: all 73 NINA regression/contract tests pass, including 32 hub-client
checks (`artifacts/hub-dotnet-all-tests.log`). Final targeted checks also pass after
fully qualified path and final deadline validation (`artifacts/hub-dotnet-final-tests.log`). Both
net48 x86/x64 runtime fixtures pass (`artifacts/hub-net48-tests.log`), and both
shared library targets build with warnings denied. The new net48 fixture script
is included in the normal Windows test playbook. Scalar checkpoint bb55313 passed
both complete push/PR CI runs, including all four portable platforms and Windows.
No hardware was actuated, and disposable host processes were cleaned up.

Next: native NINA Switch/SafetyMonitor/ObservingConditions providers and shared
native setup, then isolated COM imports, native ASCOM outputs, broader proxies and
coordination, reconnect/resume, conformance, hardware trials, documentation/site
updates and final audit/merge. This checkpoint does not close those gates.

## Native NINA scalar outputs and saved selections

The native providers implement the interfaces shipped with NINA.Plugin
3.2.0.9001. Reviewed the pinned NINA source at commit
[`2393eae581145ed5b8114bf07c48ca2580540fd5`](https://github.com/isbeorn/nina/tree/2393eae581145ed5b8114bf07c48ca2580540fd5):
ISwitchHub/ISwitch/IWritableSwitch, ISafetyMonitor, IWeatherData and their view
models. This is interface/behavior research; no upstream implementation was copied.

1. Equipment enumeration must read saved bindings only. No network/device discovery
   or host launch occurs in GetEquipment. MEF exports use the three exact NINA
   device interfaces. Choices retain instance/output UUIDs independently of labels,
   file paths and list order; a configuration choice remains available for setup.
2. Check a saved instance before launching the attachment helper, then check the
   live catalog's output UUID/class before acquiring its private client lease.
   Clone mutable caller selections before awaits. Missing/changed identities fail
   instead of falling back to another output. Setup discovery intentionally has no
   previously selected instance; saving is an explicit selection.
3. Keep connection initialization private until metadata is ready. A disconnect
   cancels the entire attempt, including the gap before session attachment. Reject
   overlapping transitions. Cancellation callbacks must run outside lifecycle
   locks; tolerate the attempt CTS being disposed after an already completed wait.
   Dispose closes only this private client and never kills the shared host.
4. Fence channel objects and getter contexts with the connection epoch. An object
   retained by NINA across reconnect cannot read/write a newly mapped session.
   Host tombstones preserve channel IDs after durable apply. Publish an immutable
   collection, including removed slots, instead of compacting the NINA list.
5. Read safety from the host on every getter; local failure/disconnection is unsafe.
   Weather failures remain per metric and return NaN. A timeout injection resets
   the source generation, so it cannot prove expiry alone. The strengthened test
   uses actual HTTP 503 responses: generation remains unchanged, the failed-cycle
   threshold is not reached, and the host policy becomes stale while weather works.
6. NINA SwitchVM polls channels but ignores a false Poll result. Its completion loop
   checks `Math.Abs(Value - TargetValue) > tolerance`, where NaN would wrongly look
   complete. Writable Value therefore throws when readback is invalid. SetValue
   invalidates the old sample and sends once; uncertainty remains an error and the
   host latch prevents another command. Target rounding matches the configured
   Rust grid, anchored at minimum with ties away from zero.
7. Do not hold a channel lock across I/O. Version poll results against writes so a
   pre-write response cannot restore an old value. Reject overlapping local writes;
   the shared host remains the authority for control, permissions and source limits.
   Capability failures create a read-only channel with a reconnect diagnostic;
   unrelated Weather/Safety outputs remain available. Initial targets come from
   cached readback and never send a command.
8. Frontend bindings store identities only, with bounded strict JSON, unique IDs,
   an OS file lock, revision compare-and-swap and flushed atomic replacement.
   Do not serialize the computed NINA Id. Reject null/invalid selections and
   unsupported fields. Native setup uses the existing theme, saves an explicitly
   chosen output, acquires no equipment lease and cannot overwrite unreadable state.
9. A first tombstone fixture asserted hostStatus's phase on an applyConfig result.
   Correct it to the actual applied/ready outcome, then verify the persisted update,
   renamed stable identity, removed slot and retired writable object after reconnect.

Validation: all 88 NINA tests pass (`artifacts/hub-nina-native-all-tests.log`),
including 15 native checks (`artifacts/hub-nina-native-tests.log`). Production
process tests cover native adapters without HTTP, actual Alpaca publication sharing
and independent EOF cleanup, safety/backoff, unavailable capabilities, uncertainty,
weather ages, cancellation, retarget refusal, and tombstones after durable apply.
Both shared library targets build with warnings denied
(`artifacts/hub-nina-native-build.log`); real net48 x86/x64 attachment fixtures pass
(`artifacts/hub-nina-native-net48-tests.log`). Both complete CI runs for client
checkpoint 9006a99 passed all eight jobs. No hardware was actuated.

The shared native output selector is implemented. Complete shared-descriptor native
configuration editing, selection removal/management, diagnostics and interactive
NINA acceptance remain required. Milestone 3 is not complete: COM imports and their
hung-driver isolation are still pending. All broader original gates stay open.

## Shared native configuration editor

1. Keep native setup in the existing frontend assembly and reuse the setup theme.
   A private editor client reads the host description/config/status, validates
   drafts and sends one revision-checked Apply. It acquires no equipment lease,
   starts no HTTP publisher and cannot disconnect a NINA client to save changes.
2. Schema `oneOf` is not always a tagged object: ConnectionPolicy contains
   described scalar constants. Both JavaScript and .NET readers now distinguish
   these cases, preserve per-choice descriptions/capability gates, and initialize
   an available scalar choice without inventing a `kind` property. Chrome verifies
   the actual remote-source control changes to managed and reaches a valid review.
3. Protect identity through the draft API as well as disabled controls. Scalar
   setters cannot replace records/collections containing immutable descendants;
   structural APIs create new UUIDs, while baseline lookup uses stable record IDs.
   The host's ledger remains the final authority for retired IDs/numbers and
   cross-field constraints. Unknown/hidden metadata survives in the candidate but
   does not appear in the ordinary preview; credential references are redacted.
4. Invalid scalar text must survive collapse and remain an error. Refuse structural
   redraws while errors exist so removing/reordering fields cannot discard an
   invalid draft silently. Expander events bubble; only the originating expander
   may unload its content, otherwise collapsing a child destroys its ancestor.
5. Review binds to a cloned candidate/version. Apply checks both again and sends
   once. A committed response still requires reload; a lost committed response
   cannot be cleared by another Review. A competing editor produces a conflict,
   and an active output client produces a rejection without being disconnected.
   If another commit wins after Apply, display the applied and current revisions.
6. Dispose must remain terminal under late responses or queued review requests.
   Cancel outside the lifecycle lock, retain the token source through active
   operations and cancellation callbacks, then dispose it. Closing the setup
   window closes only its private client; malformed review results require reload
   instead of enabling Apply. Cancellation of a dispatched review can retire the
   pipe and therefore requires explicit reconciliation too. A terminal cached-health
   read failure also revokes any earlier review; diagnostic reads serialize with
   the editor's review/apply operations so they cannot restore a stale review.
7. Use lazy controls and 32-item pages, expandable readonly identities and a
   single cached-health source picker. Do not create a button for every source
   or pre-render every channel. Live inspection and richer policy diagnostics
   remain separate setup refinements.
8. The first WPF render returned transparent pixels despite a passing workflow.
   Inspecting the pixels prevented treating an empty screenshot as evidence.
   WPF throttles rendering when a desktop session has no display; the
   [upstream compatibility guidance](https://github.com/dotnet/wpf/issues/2811#issuecomment-604764804)
   documents a switch for short renders. Enable it only in the test runtime,
   render the actual laid-out WPF content and reject empty captures. Production
   rendering settings are unchanged. The verified images show automated
   simulation, not interactive NINA or hardware acceptance.
9. Push CI for 8e73b27 exposed the deliberately stalled writer in the queued
   cancellation ordering test hitting its 2-second fixture frame deadline.
   Increase that fixture to 10-second frames/20-second requests. Separate deadline
   tests retain their short bounds, and production transport limits are unchanged.
   PR CI for the same head passed all eight jobs; do not describe the failed push
   as a green checkpoint.

Validation: all 109 NINA tests pass (`artifacts/hub-native-editor-all-tests.log`),
including 21 additional draft/editor/window checks. The real production-host
fixtures cover saved edits, field errors, competing revisions, active clients,
and no equipment leases. Fault tests cover committed reply loss, malformed
validation, health transport failure and disposal while requests wait. The actual WPF window executes
edit/review/apply/saved-health and host-survival checks; reviewed renders are in
`docs/images/hub-native-*-simulation.png`. net48 x86/x64 public editor fixtures
pass (`artifacts/hub-native-editor-net48.log`), as do warning-denied net48/net8/NINA
builds (`artifacts/hub-native-editor-build.log`), 29 Alpaca checks after rebuilding
the web resources, JavaScript contract/draft checks and four independent JSON
Schema tests. Chrome verifies the actual scalar policy picker and host validation
(`artifacts/hub-web-policy-reviewed.jpg`). Its disposable publisher/host were
stopped after checking their executable and unique config path. No hardware was
actuated. This increment still requires complete CI and interactive acceptance.

Next: native setup initialization/credential management/inspection/simulation
controls/selection management, interactive NINA acceptance, isolated COM imports,
native ASCOM outputs, broader proxies and camera/coordination contracts, recovery,
conformance and real-device trials, README/site/screenshots and final audit/merge.
The full original plan remains in scope; milestone 3's complete gate is still open.

## Isolated Windows COM worker boundary, 2026-10-06

Reviewed the first three import interfaces against the pinned
ASCOM.DeviceInterfaces/Exception.Library 7.1.2 declarations. Use the existing
bounded accessory response envelope; do not add an HTTP hop or per-class host.
The one hub ASCOM project reserves future native-output server work and builds
distinct x86/x64 workers with separate intermediate directories.

Findings and corrections:

1. Defer activation until the first connect step so the Rust parent can attach
   its process ownership guard first. The input reader dispatches serial requests
   to one STA with a WPF message pump; activation/calls/RCW release share that STA.
   Real COM fixture traces verify thread/apartment/bitness and a queued callback.
2. Do not let reflection turn a caller-supplied name into an arbitrary operation.
   Whitelist and type-check members/parameters; connection changes belong only
   to handshake operations. SetupDialog, Action/Command and Dispose cannot be
   reached by ordinary imports. Ignore vendor Console output, but any native
   stdout corruption must still retire the parent transport.
3. Negotiate legacy versus modern ownership. Borrow legacy/global connections
   that were already open; claim the modern private connection even if shared
   hardware is connected. Never fall back from rejected modern Connect to a
   Connected setter. EOF cleanup consumes owned Disconnect once, without replay.
4. Initial review found failed verification blocked cleanup of an acknowledged
   connection. Permit Disconnect after a definitive verification failure while
   continuing to reject uncertain connection changes. A fixture verifies owned
   cleanup runs once; uncertain Connect/Disconnect tests prove no replay.
5. A generic failed mutation can follow a hardware change. Return uncertain and
   latch later worker writes; permit diagnostic reads. The shared actor must
   still enforce its existing control leases, uncertainty latch and generation
   fences when it adopts this worker. The worker does not replace that policy.
   Only internal pre-dispatch checks and the defined ASCOM InvalidValue HRESULT
   prove rejection; an arbitrary vendor ArgumentException remains uncertain.
6. Preserve known ASCOM HRESULTs, including missing/unsupported/not-connected
   and per-sensor unavailable errors, without returning arbitrary messages or
   stack traces. Invalid/nonfinite input is rejected before dispatch, and
   non-Boolean safety data is unavailable rather than coerced to permission.
7. Treat malformed frames, duplicate keys, non-increasing IDs and unsupported
   protocol versions as terminal. Bound requests/response size and parser depth.
   EOF releases only acknowledged ownership and never calls vendor Dispose,
   which can disconnect globally shared equipment.
8. Use real private HKCU COM registration, not a production fixture activation
   hook. Fail if the private CLSID/ProgID already exists; remove only those exact
   registrations in finally. Explicit pointer-sized Win32 arguments keep cleanup
   correct in a 64-bit Python host. Test fixture configuration/hooks reside only
   in the fixture assembly, and no installed hardware driver is activated.

Validation: `scripts/test-hub-com.ps1` passes both warnings-denied worker builds,
the AnyCPU fixture build, and 16 test cases exercised in both x86/x64, logged in
`artifacts/hub-com-worker-tests.log`. Actual COM cases cover metadata, switch
writes, legacy/modern ownership, safety types, weather ages/canonical names/
per-sensor errors/Refresh/AveragePeriod, sanitized HRESULTs, command uncertainty,
EOF cleanup, missing registration, wrong bitness, malformed frames and stalled
worker isolation. Test registration is removed afterward. No hardware is moved
or disconnected. The normal Windows playbook now runs these fixtures.

Native-editor checkpoint 2171bec passed complete push CI 37432217328 and PR CI
37432225426 (all eight jobs each). This worker increment requires its own CI.
Rust factory/adapter adoption, process-guard/deadline/cancellation and generation
tests, cached safety expiry during a COM stall, mixed COM/native/network outputs,
private payload/signing, native ASCOM outputs and every original remaining gate
remain required. The host still rejects COM sources and does not advertise them.

## Rust COM adoption and private payload, 2026-10-06

Reviewed the Rust factory, parent transport, actor generations and shared scalar
polling against the preceding worker contract. COM sources now use the same
runtime and output policies as native/Alpaca sources. No installed equipment
driver is activated by these tests.

Findings and corrections:

1. Deserialize typed inner replies directly from bounded frame bytes. Converting
   first to a generic JSON object would discard duplicate fields. Retire corrupt
   framing, wrong identity/types or contradictory connection ownership; preserve
   null versus absent fields and complete signed HRESULT values.
2. Attach worker ownership before activation. The existing kill-tree job is
   correct for direct hardware workers, but a COM driver can spawn a shared vendor
   helper. Add an explicit independent-worker policy with silent descendant
   breakaway. The real child-process fixture proves parent cancellation kills
   the private worker while its helper stays alive; the test ends that helper
   through its own stop marker.
3. Arm connection uncertainty before awaiting a managed handshake. Cancellation
   must not permit a replacement worker to replay Connect or Disconnect. Bound
   the total handshake as well as each RPC: a driver can return Connecting=true
   forever without individually timing out. Incomplete cleanup also remains
   uncertain after reset.
4. A lost write retires the worker, but does not clear the actor's uncertainty
   latch. A real fixture records one dispatch before stalling, then proves a
   replacement generation cannot replay it or accept a later mutation. Normal
   read faults may recover in a new private worker, without claiming confirmed
   upstream cleanup or terminating a vendor server.
5. Factor scalar plans, one-request polling, same-key retries, weather ages and
   per-sensor failures into `sampling.rs`, shared with Alpaca. Safety retains its
   single typed IsSafe observation rather than counting getters as evidence. A
   COM stall test expires cached permission while the blocked worker is still
   alive, without resetting its generation; unrelated weather remains usable.
6. Capability metadata must reflect installed x86/x64 helpers and the three
   supported COM classes. Both generic setup readers disable unimplemented
   classes/architectures. Configuration preparation checks paths/types without
   activation, discovery, simulation substitution or connecting equipment.
7. Stage both helper architectures, all runtime DLLs/config and dependency
   licenses. Reuse the existing .NET license extraction for ASCOM and COM
   payloads. The ASCOM package refreshes the helper tree after signing; release
   signing/verification lists both EXEs. ZIP validation checks worker presence,
   assembly version and architecture. A local variable collision initially
   broke the validator build and was corrected before package acceptance.
8. Foundation CI 1bc1d7b failed Windows activation before any fixture trace with
   HRESULT 0x80070002; all seven other jobs passed. Add fixture-only merged
   registry/direct managed-load diagnostics after failures. Do not weaken the
   production error boundary, add a production activation bypass or infer the
   runner root cause from successful local tests. A local diagnostic invocation
   initially hit Windows PowerShell execution policy; its test-only subprocess
   now uses scoped Bypass, and both probes pass. Run fixture registration
   playbooks sequentially: they intentionally share one private fail-if-present
   CLSID, and overlapping probes can remove another test's registration. A clean
   sequential staged run passes; registration is removed afterward.

Validation: `artifacts/hub-com-parent-tests.log` and
`artifacts/hub-com-staged-tests.log` pass 16 worker cases plus ten Rust-parent
cases, each exercised in both architectures. Parent cases include modern/legacy
ownership, weather ages/partial errors, factory sharing and last disconnect,
cancelled reads, surviving vendor child, lost writes, uncertain connections,
independent safety expiry, mixed native-simulation/loopback-Alpaca/COM gauges,
corrupt stdout and permanently pending cleanup. Staged helpers use the actual
package paths and dependencies. Clippy, full core/hub/Alpaca tests, Rust 1.89,
110 NINA tests, JavaScript/schema checks and net48 x86/x64 fixtures pass locally.
Unsigned NINA/ASCOM packages validate (`artifacts/hub-com-package-retry.log`,
`artifacts/hub-com-ascom-package.log`). Both local loader probes pass
(`artifacts/hub-com-loader-probe.log`). Updated CI and signed release validation
remain required; local success does not resolve the observed runner failure.

Runner follow-up: adapter 9dfdb82 failed Windows in push 37438671971 and PR
37438682468; the other seven jobs passed in each run. The probes show both direct
managed loads succeed, with failed COM class-factory activation in elevated
32/64-bit PowerShell. HKCR displays the correct private keys and codebase, so
its merged view does not prove elevated COM can load them.
[Microsoft's elevated COM guidance](https://learn.microsoft.com/en-us/windows/win32/com/the-com-elevation-moniker)
explains why per-user registration is insufficient. The test playbook now
explicitly selects private HKLM registration only on elevated disposable GitHub
Windows runners; Python verifies both environment and elevation. Preflight checks
both hives and architectures, and finally removes only the exact private keys
created. Local HKCU runs still cover both worker/parent architectures, and the
machine option is rejected locally. Keep ProgIDs within the
[documented 39-character bound](https://learn.microsoft.com/en-us/windows/win32/com/-progid--key),
including fixture aliases. Production activation and error sanitization are
unchanged. This correction still requires new CI evidence.

## Common native saved-choice management, 2026-10-06

Added removal to the shared selection store and the same themed selector used by
native setup. This is a user-scoped chooser operation, not deletion of a hub
output or source. Review focused on independent client leases, configuration
identity, concurrent saves and uncertain outcomes.

1. Save and Remove share one bounded, revision-checked atomic update path and
   persistent OS lock. Remove identifies both instance and output UUID; missing
   identities and stale revisions do not produce a new saved revision. Empty
   chooser lists remain valid and retain their revision for subsequent saves.
2. Never attach to a host or revoke equipment leases from the manager. Connected
   native devices retain their private selection and session. Rescan enumerates
   the reduced chooser list; explicitly saving the output restores the same ID.
   Production-host and actual net48 tests verify connected leases survive removal.
3. A failed/conflicting removal disables mutation until explicit reload. The
   selector reconciles its own revision after returning from management; another
   later save still fails CAS. An unreadable selection file cannot be overwritten.
4. Show label, simulation marking, config path and both identities. The first
   render clipped the long path/instance detail horizontally; reviewing it led
   to wrapping row content and disabling the horizontal scrollbar. The verified
   actual WPF render is `docs/images/hub-native-selections-simulation.png`, clearly
   labeled automated simulation. No installed NINA/ASCOM acceptance is claimed.

Validation: all 114 NINA checks pass (`artifacts/hub-selection-all-tests.log`),
including four new store/live-lease/WPF cases. Tests cover competing saves,
missing/empty identities, unreadable files, empty lists/restoration, actual chooser
enumeration and continued production-host leases. net48 x86/x64 public fixtures
exercise the same removal/CAS/retained-lease behavior
(`artifacts/hub-selection-net48.log`); both native framework builds pass with
warnings denied (`artifacts/hub-selection-build.log`). New CI remains required.

## 2026-10-06: typed native ASCOM outputs and common COM server

Reviewed current/legacy interface metadata against the pinned ASCOM 7.1.2 assembly
and async completion/error semantics against the primary interface documentation.
The existing helper serves both isolated imports and bound native outputs; no
per-class executable/project was added.

1. Separate verified host attachment from equipment acquisition. ASCOM delegates
   modern/legacy connection operations to the Rust host and retains failures in
   Connecting. Admission is reserved before dispatch; no automatic retry occurs.
   Each COM object checks required interface capabilities before acquiring equipment
   and retains a private client and immutable selection. Dispose and
   disconnect preserve sibling leases. Added logical connection epochs as well
   as transport epochs so responses cannot become fresh after a connection change.
2. Convert scalar values to current/legacy ASCOM interfaces, including DeviceState
   collections. Unavailable weather raises ValueNotSet, unimplemented properties
   and methods have distinct ASCOM errors, and invalid/non-finite writes fail.
   General source failures retain full HRESULTs with sanitized diagnostics.
   No measurement TimeStamp is synthesized. Review fixed stale simulation display
   metadata by replacing it from the authenticated catalogue on attachment.
3. Derive dynamic CLSIDs with UUIDv5 from instance/output/class and 39-character
   ProgIDs. An independent Python implementation verifies the actual factory IDs
   through COM activation. Four outputs include two Switch outputs sharing a
   source; names/order do not establish identity. Register fixture keys only after
   both-hive/bitness preflight and remove only exact private keys in finally. Machine
   fixture registration remains restricted to elevated disposable GitHub runners.
4. Extract the pumping STA, metadata warmup, weak object tracking, factory/QI and
   shutdown into LocalComServer shared by hub, Pegasus, OFP2 and ETA frontends.
   Actual 32/64-bit COM simulation regressions pass for the existing drivers.
   Shared native setup opens the same editor; interactive COM SetupDialog and
   production registration/SCM lifecycle remain pending.
5. Checkpoint 64178eb passed all eight jobs in both CI runs, verifying the private
   HKLM runner correction. Subsequent 1adcafe PR CI found the queued-writer fixture
   still used the peer's short default deadline despite a longer client deadline.
   Give only that deliberate 100 kB/tiny-buffer exchange a bounded ten-second peer
   allowance; production limits and separate deadline/fault tests are unchanged.
   The earlier safety fixture fix tolerates only expected socket abort/reset/EOF.

Local evidence: all 117 NINA checks (`artifacts/hub-output-nina-tests.log`), actual
net48 adapters/current+legacy COM QI in both bitnesses
(`artifacts/hub-ascom-output-tests.log`), real exported COM dispatch to each server
architecture from both client bitnesses (`artifacts/hub-exports-com-tests.log`),
and serial-driver regressions (`artifacts/hub-shared-server-{ofp2,pegasus,eta,falcon}.log`).
The 16 import-worker cases and ten Rust-parent cases each cover both architectures
again (`artifacts/hub-output-import-regression.log`). Unsigned NINA and ASCOM
packages build with the new private assembly (`artifacts/hub-output-package.log`
and `artifacts/hub-output-ascom-package.log`).
The same export playbook passes with the actual staged helpers and release Rust
host (`artifacts/hub-exports-staged-tests.log`); publication checks pass without
remote writes (`artifacts/hub-output-release-checks.log`).
The new private frontend DLL is explicitly signed/verified alongside both helper
EXEs; package validation requires its version and architecture. Signed development
payload validation remains pending.

Next: new CI and remaining production registration/removal, identifiable export
self-proxy/alias checks, setup/SCM/conformance acceptance, then every original
broader proxy, camera/acquisition, coordination, recovery, hardware and
README/site/screenshots/final merge gate. PR #21 remains draft.

## 2026-10-06: native export aliases and startup diagnostics

1. Configuration rejects canonical native ASCOM self-proxy names case-insensitively.
   Rust UUIDv5 and the existing .NET identity algorithm agree on fixed vectors for
   all three scalar classes. Renames and output order do not retarget an identity;
   different hub instances remain allowed. The .NET helper moved unchanged to the
   common frontend assembly so registration can use the same implementation.
2. The production factory supplies all configured output class IDs to the isolated
   COM worker. Resolve the actual registry binding before activation and activate
   the checked CLSID. Review/testing rejected Type.GUID: a registered managed alias
   can expose its managed class GUID instead of the alias's registered CLSID.
   Lookup follows bitness and elevation; no driver constructor or automatic ProgID
   installation runs as part of self-proxy rejection. Rejected sources report an
   invalid value and empty readings while scalar clients retain diagnostics.
   This covers local aliases, not arbitrary cross-host dependency cycles.
3. Both native-output CI runs for 779737f failed Windows export condition waits.
   Existing logs hid the second client's failure. Collect both peer outputs and
   name each wait without increasing the deadlines or swallowing COM exceptions.
   The fixture passes locally; the runner-specific cause remains unresolved.
4. Push CI also failed macOS Intel process startup at the strict socket permission
   check. Inspection found bind exposed the public inode before chmod. Stage the
   socket privately on the same filesystem and publish after mode 0600 is set,
   retaining peer admission, stale-path checks and inode-based cleanup. Added an
   actual renamed-socket exchange and rejection after broadening permissions.
   This Unix test awaits portable CI; local cross-target checking could not proceed
   because the Linux C compiler required by ring is absent. No security check was
   weakened and no global umask was changed.

Local evidence: `artifacts/hub-export-alias-tests.log` has 17 registered worker and
11 Rust-parent cases passing in both architectures, including zero activation
calls for a registered self alias and repeat connection. Cross-language config
vectors pass (`hub-export-identity-tests.log`, `hub-export-identity-dotnet.log`).
The hub/Alpaca suites pass (`hub-export-guard-rust-tests.log`), along with 117 NINA
checks (`hub-export-guard-nina.log`), strict Clippy and Rust 1.89 checks. Real export
fixtures pass after the diagnostic changes (`hub-export-guard-diagnostics.log`).
Unsigned NINA and ASCOM packages validate (`hub-export-guard-package.log`,
`hub-export-guard-ascom-package.log`). No installed equipment was activated.

Next: verify portable publication and diagnose Windows using the new peer logs,
then production registration/removal and every original remaining gate. This
checkpoint does not close native ASCOM acceptance or the broader plan.

## 2026-10-06: explicit bound launch and strict collection checks

f94c85c push CI 37447788480 and PR CI 37447794795 passed all four portable platforms,
packages, research and COM activation. Both Windows jobs failed the export test.
The new peer logs show the second client failed DeviceState, while the first only
timed out waiting for its completion signal. This is not a connection deadline
failure and increasing waits would not address it.

Review found the PowerShell property helper returned enumerable properties through
the pipeline, losing the collection wrapper. Preserve the original value with a
non-enumerating return, including through the export fixture's wrapper. Actual COM
tests now use reflection for Count and Item, and verify IsSafe's name and boolean
false value. Both framework client bitnesses pass locally; helper regressions also
preserve empty and single-item arrays. New CI must confirm the original failure is
resolved; local installed ASCOM metadata alone cannot establish a runner result.

The export helper accepts explicit absolute binding and host paths for SCM commands,
validates file presence/duplicates/relative paths before factory publication and
ignores inherited binding/host values for an explicit registered launch. Manual
bound fixtures prove metadata does not start a host, then exercise four outputs
with both client/server bitnesses and independent leases. Warnings-denied helper
builds pass (`artifacts/hub-bound-launch-build.log`), and the actual exchanges,
collection members, invalid-argument cases and CIM/OS-handle process identity
checks pass (`artifacts/hub-bound-manual-tests.log`).

Cold SCM activation using private HKCU entries fails locally with 0x80040154 before
any startup phase is written, despite a matching merged registry entry. Native
CoGetClassObject also failed during diagnosis; no activation bypass is retained.
The production machine-registration case remains unverified. Add a private HKLM
SCM variant on the disposable runner, following the existing elevated fixture
guard, AppID/Interactive User model and ServerExecutable registration. That test
does not activate installed equipment entries. It discovers only the unique fixture
binding command, verifies creation time and executable through an opened process
handle before termination, and attempts every exact-key cleanup even when a child
or diagnostic operation fails. All entries still have collision preflight in both
hives/views. Local tests keep HKCU; they never attempt elevation or machine writes.

Next: corrected Windows CI and cold machine launch, then production ownership/
registration/removal, interactive setup and every original remaining plan gate.

## 2026-10-06: production registration and ownership foundation

d51abac push CI 37449772668 and PR CI 37449781613 passed all eight jobs.
Windows logs verify strict DeviceState values and cold SCM launch from both
client bitnesses against both server architectures. Unix publication passes all
four platforms. The collection wrapper and socket publication failures are
resolved at that checkpoint; production registration is a new increment.

Review of registration/removal addressed:

1. Use the existing helper/shared identity rather than adding an executable or
   duplicating GUID derivation. Both chooser views launch the installed x64
   helper with explicit paths and original SID. Different-account elevation and
   activation fail before factories/equipment acquisition.
2. Hold the selection CAS writer lock and machine registration mutex through
   preflight/publication. A second-view collision, user overlay, changed command,
   foreign owner/install or newer version cannot authorize a first-view write.
3. Persist pending ownership before keys and ready after publication. Explicit
   retry can repair owned partial entries. Removal uses inventory after selection
   deletion and installer removal is scoped to the exact installation.
4. Capture registry kinds and custom owner/group/DACL/protection for rollback.
   Ordinary read/write handles lack security-write rights; reopen only the same
   key with required rights, enabling no privileges and broadening no ACL.
   SACL/audit metadata and process-crash atomicity are not claimed. Attempt every
   restoration and report partial rollback rather than hiding cleanup errors.
5. Bound the whole snapshot to 512 keys, depth 16 and 1 MiB, before any mutation.
   Oversized/deep trees leave both views untouched. Fixture cleanup also accepts
   missing parents after a failed registration but still reports other failures.

Local evidence: all 126 NINA tests pass, including nine private-registry cases
(`artifacts/hub-registration-nina-tests.log`). net48 x86/x64 selection/editor/
adapter fixtures pass (`hub-registration-net48-tests.log`); the registration
helper builds with warnings denied (`hub-registration-build.log`) and unsigned
ASCOM package validates (`hub-registration-package.log`). Owner-mismatch and
manual COM fixtures passed earlier (`hub-registration-owner-tests.log`). No
installed hardware driver was activated. A new disposable-runner-only fixture
uses the actual helper for publication, cold SCM activation and inventory-based
removal after deleting selections; its result is still pending new CI.

Next: production fixture CI, themed registration manager, installer lifecycle,
conformance and all original remaining plan gates. The backend/CLI alone does
not complete milestone 4's native setup or wider proxy/coordination requirements.

## 2026-10-06: themed ASCOM registration manager

The existing setup executable now opens the shared themed manager with `/hubsetup`
and a Start menu shortcut. It reconciles saved choices against both registry
inventories, including orphaned entries. The existing native selector/editor
serve all scalar classes; initial manager load never opens equipment or a host.
Immutable registration requests carry the observed selection revision and original
SID to the installed helper through Windows elevation. Foreign ownership, install,
selection file or newer version disables register/remove. Removing a saved choice
requires registration removal first. While waiting, all competing actions are
disabled; errors and unknown completion require reload, with no replay or helper
termination. Closing the window cannot revive controls or start another operation.

Review expanded installer in-use checks to cover nested hub executables/DLLs.
It also found dynamic registrations are not included in Inno's generated registry
table, and `/unregserver` is not the installer uninstall path. The backend's manual
inventory-removal command therefore does not prove installer cleanup. Add the
proper uninstall lifecycle hook and failure/preservation tests before that gate.

All 129 NINA tests pass (`artifacts/hub-registration-ui-tests.log`), including
three actual WPF workflows using private registry roots and no real elevation.
They cover registration, orphan removal, foreign-owner protection, unreadable
settings, busy admission, timeout/no-replay and explicit reload. Actual rendered
simulation screenshot is `docs/images/hub-ascom-registration-simulation.png`;
it was visually inspected for theme, controls, wrapping and simulation marking.
net48 x86/x64 fixtures pass (`hub-registration-ui-net48.log`), the existing setup
helper builds with warnings denied (`hub-registration-ui-build.log`), and unsigned
ASCOM packaging validates (`hub-registration-ui-package.log`). These do not prove
interactive UAC, installed Chooser, installer lifecycle or conformance acceptance.
Unsigned installer compilation also passes using the existing pinned compiler
(`hub-registration-ui-installer-build.log`); no installer was run on this machine.

Next: production fixture CI and installer lifecycle, then complete remaining
setup, broader proxies/cameras/coordination and every original acceptance gate.

## 2026-10-06: installer lifecycle and complete-install cleanup

b76d2fc PR CI 37452667674 and push CI 37452661961 failed Windows; the other seven
jobs passed. The PR log shows `UnboundLocalError` for `created` in the new
registered-export fixture, before helper publication. Review found that block
also skipped collision preflight and sat outside finally. Move registration
inside the guarded cleanup after all hives/views are checked; reserve exact paths
only then, skip raw fixture writes for this mode and retain collision refusals.
Local manual COM exchanges still pass (`hub-registration-fixture-order-local.log`).
Production helper activation/removal must be proved by new CI, not these manual
tests. 434db2f inherits the fixture fault until this correction lands.

Inno's pinned [6.7.3 uninstall source](https://github.com/jrsoftware/issrc/blob/is-6_7_3/Projects/Src/Setup.Uninstall.pas)
calls `usUninstall` after confirmation and invokes it with fatal exception handling
before `PerformUninstall`. Wire owned inventory cleanup into this event; nonzero
helper completion aborts before file deletion. Do not unregister on initialization
or cancellation. The existing in-use guard still runs first and never stops clients.

Complete-install removal now preflights every output and both views before any
deletion, with one bounded rollback batch. A later conflicting output cannot
leave an earlier one removed. Caught failures attempt every restoration, including
custom security. A foreign install's future schema is skipped by its installation
marker; this helper must not interpret or remove it. Individual snapshot limits
remain; batch limits are 256 outputs, 4096 keys and 8 MiB, and inventory enumeration
admits 4096 entries. Partial restoration and crash limits remain explicit.

The disposable-only actual installer fixture registers three scalar outputs plus
a separate real helper/payload with another identity. It checks same-directory
upgrade preservation, failed uninstall on a changed command with all files/entries
retained, removal after deleting the selection file, and preservation of the other
install/settings. Cleanup uses owned helpers, not raw key deletion. Paths stay
inside the named installer fixture directory; manifests are written only after
collision preflight. Even a partially failed preparation attempts cleanup, and
cleanup errors do not skip restoring the existing test environment. The initial
lifecycle fixture does not activate COM or acquire equipment. The extension below
adds only private installed metadata activation, with no equipment connection.

Local evidence: 131 NINA tests passed (`artifacts/hub-installer-batch-tests.log`);
all 11 registry tests passed after final scope refinement
(`hub-installer-registry-review-tests.log`). net48 x86/x64 fixtures pass
(`hub-installer-net48-tests.log`), unsigned packaging validates
(`hub-installer-package.log`) and the hooked installer compiles
(`hub-installer-build.log`). Python and PowerShell syntax checks pass. The new
machine fixture refuses local invocation before filesystem/registry mutation
(`hub-installer-local-guard.log`). No actual installer was run locally and no
installed equipment was activated. Actual machine/UAC/conformance acceptance
and every remaining original milestone still require evidence.

Next: corrected production and installer CI, then remaining shared setup,
broader typed proxies/camera acquisition, coordination and complete original gates.

Installer acceptance now also holds one actual installed hub COM object in the
PowerShell 7 parent without Connect. Its nested helper/shared DLL must block
upgrade and uninstall; the fixture then checks inventories are intact and no
Rust host started. After releasing only its own RCW, it waits for natural idle
retirement before continuing maintenance. It never kills another client's helper.
Python/PowerShell syntax checks pass; this extension needs new machine CI and is
not evidence of a completed in-use/installed metadata gate yet.

### Shared native credentials and known-reference reconciliation

NINA and ASCOM share one credential tab in the existing native editor. Labels,
descriptions, lengths, protection text and reference prefix come from the host.
The password input is never prefilled or placed in configuration/review. It is
cleared before dispatch and on reload/close; managed strings and JSON/OS copies
are not claimed to be securely erased. Returned shapes are checked strictly;
an unexpected secret-bearing member fails protocol validation without exposing
that response through the editor API.

Creation accepts an optional caller-chosen non-nil UUID. The frontend retains
its reference before dispatch, and old callers omitting the ID remain supported.
Persistence never overwrites a record and never interprets a duplicate as replay
permission. Unknown transport, malformed reply or unavailable storage outcomes
disable further mutation until explicit reload/status. The window preserves the
reference across reloads, but not across closing the window. Credential mutations
invalidate a prior configuration review; apply still validates through the host.

Review found that status could race a pending create/delete and report absence
before an abandoned write completed. Status now acquires the same transaction
gate, returning busy while storage or configuration work is pending. The blocking
task retains that gate if the request waiter disappears. No automatic polling,
recreation, deletion or Apply is introduced by setup.

Local evidence: four Rust credential integration cases, including abandoned reply,
read-only reconciliation, duplicate/no-overwrite, nil rejection and transaction
admission, pass (`artifacts/hub-known-credential-rust.log`). Full Rust hub/Alpaca
suites pass (`hub-credentials-rust-suite.log`), as do Clippy and Rust 1.89 checks.
All 136 NINA checks pass (`hub-credential-full-nina.log`), including production
host storage without equipment leases, uncertain/malformed/remote-unavailable
response reconciliation, review invalidation and the actual WPF secret-clear/
reload/removal workflow. net48 builds and x86/x64 client fixtures pass. The actual
credential render was inspected; documentation marks simulated equipment.
Web credential setup and interactive frontend acceptance remain required.

CI checkpoint e728f55's Windows test step proves actual owned-helper publication,
both-bit SCM clients and removal with deleted bindings; seven other jobs pass.
The installer phase fails before setup at a registry key with no PlatformVersion
(`artifacts/hub-metadata-ci-failure.log`). The fixture now tests value presence,
deletes missing values without throwing and restores original kinds, using
explicit writable handles. Parser/local pre-mutation rejection pass; actual
installer lifecycle and nested helper in-use behavior still await CI. A temporary
local registry-value probe was rejected by automatic approval review with
"blocked by policy"; no result from that probe is used as validation.

### Web credentials and independent setup reattachment

Web setup now uses the host's write-only authorization and reference descriptors,
including accurate protection text. It retains a chosen reference before dispatch,
clears the masked input and never puts values into draft/review/diagnostics. Public
response shapes are checked before consumption. Lost/malformed/unavailable replies
invalidate write admission until explicit Reload/status; pending requests refuse
competing changes. Local validation errors send no request. Unicode reference
limits count scalars and reject unpaired surrogates. Secret JS/JSON/OS copies are
not claimed to be securely erased or stored in browser persistence.

Review found the permanent HTTP catalog stream also served setup, so Reload could
not recover a failed transport without restarting the whole frontend. Setup now
owns a separate private stream and explicit POST Reload with an empty object and
128-byte limit. It uses the same same-origin/JSON/Fetch-Metadata checks as ordinary
setup, shares bounded request admission, fences shutdown and replaces only its
own stream. It never starts the host, touches equipment leases, replaces the
catalog or replays an unknown write. Startup checks that both streams identify the
same host. Host replacement remains an explicit equipment recovery concern.
The existing host-capacity test needed to reserve both catalog and setup streams;
its expected rejection/retained failure/reuse checks still pass.

Local evidence: full Alpaca tests pass (`artifacts/hub-web-credentials-rust.log`),
with final library/HTTP checks after review in `hub-web-credential-final-rust.log`.
They prove closed setup recovery with a surviving catalog, busy/shutdown admission,
cross-origin/Fetch-Metadata/media rejection, strict empty Reload bodies, shared
credential storage/configured-reference protection and surviving equipment leases.
JavaScript tests in `scripts/test-hub-config.mjs` cover lost/malformed/storage errors,
reference retention, no replay, invalid-input admission, review invalidation,
pending changes and scalar limits. Clippy and Rust 1.89 checks pass; 136 NINA
regressions pass (`hub-web-credential-nina-regressions.log`).

The actual browser against a copied production executable proves password clearing,
revoked review, retained reference through Reload, protected status after an own
fixture host restart, explicit removal/absence, zero console errors and zero
equipment leases with unchanged configuration. Recorded evidence is
`artifacts/hub-web-browser-verification.json`; the actual screenshot is documented
as simulation. Both own fixture processes were stopped after identity checks, and
the disposable credential was removed. No installed equipment was activated.

Native-credential checkpoint 2a26281 passes all eight jobs in PR run 37457955380.
`artifacts/hub-credentials-ci-windows.log` proves the actual installer phases:
prepare/assert, installed metadata without host startup, nested helper busy guards
and idle retirement, upgrade preservation, break/failed uninstall/repair/assert,
deleted bindings, removed inventories preserving the second install/settings and
owned cleanup. This closes those automated fixture checks, not interactive UAC,
Chooser, conformance, vendor hardware or the original broader milestones. The new
web increment still requires its own CI.

### Shared native inspection and diagnostics export

The native editor now uses explicit paged inspection through the existing host
path. Parameters and deadlines come from the same descriptors as web setup. Its
saved-source selector cannot silently target a new unsaved draft source. Inspection
revokes review before dispatch, preserves sibling connection leases, rejects stale
source/revision/generation and malformed cursors, and never replays after a lost
reply. Cancellation invalidates further writes; disposal cannot restore session
state. Per-property unavailable/unsupported results remain visible. No standalone
probe establishes live safety permission.

Review found clearing the preview before session admission could leave an accepted
review enabled after local invalid input. The window now clears only after review
is actually revoked; the WPF regression verifies invalid input preserves review
and valid inspection revokes it. Export uses public saved host status and the last
completed source observation with explicit time/revision. It excludes the draft,
configuration, credential values and credential calls. The production credential
test verifies its disposable secret/reference cannot enter this export. Reload
clears prior observations. File selection remains an explicit user action.

Local evidence: all 144 NINA checks pass in
`artifacts/hub-native-inspection-final-nina.log`, including production Switch
pagination, safety/weather inspection, sibling-lease preservation, lost/obsolete/
malformed replies, local descriptor limits, cancellation and the actual WPF flow.
The shared net48 warnings-denied build and actual x86/x64 inspection/export and
ASCOM client fixtures pass (`hub-native-inspection-net48.log` and
`hub-native-inspection-net48-fixtures.log`). The actual WPF simulation render was
inspected. Broader diagnostics, initialization, simulation controls, interactive
acceptance and every remaining original milestone stay required. Checkpoint
2a26281 now passes both complete eight-job CI runs; web/native-inspection CI must
still be checked to terminal completion.

### Shared native and web simulation setup

Reviewed the shared descriptors, native/WPF session, browser state model and
production IPC/HTTP paths. Controls are limited to saved explicit simulated
sources and sparse selected fields. Weather physical limits use the same backend
validation; defaults and channel names derive from actual simulation. Both editors
fence configuration revisions, validate reply identity/status, serialize operations
and revoke review before dispatch. Reading current state clears the form selection
without opening equipment; an uncertain update cannot be replayed. Safety polling
and uncertain-write reconciliation retain their existing semantics.

Production fixtures exposed a real wire bug: numeric Switch keys failed through
serde's internally tagged command capture. An explicit map decoder now parses
canonical IDs and rejects aliases, duplicates and unknown channels. Tests exercise
actual tagged JSON and protected HTTP with successful sparse updates, stale
revisions before any lease, sibling-lease preservation and unchanged configuration.
Review also found invalid typed response values could be mistaken for local input
errors in JavaScript; they now become protocol failures that block another write.

The net48 build rejected a nested record lacking IsExternalInit; a simple immutable
class avoids adding a compatibility shim. The actual net48 fixture initially left
its injected level for a later independent contract suite; it now explicitly
restores state and waits for its own lease cleanup. No deadline was increased and
no live process was restarted on an observation timeout.

Evidence: full hub/Alpaca suites, strict Clippy and Rust 1.89 checks pass in
`artifacts/hub-simulation-rust-suite.log`, `hub-simulation-clippy.log` and
`hub-simulation-msrv.log`. Generated-contract freshness, JavaScript state tests
and all four Python schema checks pass. All 149 NINA tests pass after final review;
the warnings-denied net48 build and real x86/x64 simulation/ASCOM client fixtures
pass. Browser verification records sparse level updates preserving other channels,
absent temperature and sample age, unchanged revision, no equipment leases and
zero console errors in `artifacts/hub-web-simulation-verification.json`. The actual
WPF/browser screenshots were inspected and labeled simulation. The owned browser
tab and both copied fixture processes were closed after identity checks.

Both a2cad0a CI runs are successful. Inspection checkpoint 94b9adc passes all eight
jobs in PR 37462088304 and push 37462081747. Simulation CI is required after push.
Shared initialization, broader diagnostics, typed proxies/cameras/coordination,
interactive/conformance/hardware acceptance and every original remaining milestone
stay open; this increment does not close the entire setup milestone.

### First-time persistence and inert initialization mode

Added the common Rust `ConfigStore::create` path and explicit `--hub-init` mode
before workers/SDK/host startup. Review checked absolute paths, existing-parent
resolution, same-directory flushed staging, no-clobber publication, fresh empty
identity, post-publication durability uncertainty and rejection of mixed modes.
Existing invalid files are preserved instead of treated as initialization targets.
The returned store shares the regular revision-checked durable editing path.

Tests race eight creators, verify exactly one winner and matching persisted
identity, preserve existing invalid bytes and missing-parent state, reject relative
paths and (on Unix) existing symlinks. The production executable test verifies
no live endpoint/host ownership, unchanged bytes after repeated/mixed-mode calls
and no stdout on failure. Its observation timeout is independent of the inner
probe deadline so runner scheduling cannot imply a host started. Full Rust suites,
strict Clippy and the installed Rust 1.89.0 check pass in
`artifacts/hub-initialization-rust.log`, `hub-initialization-clippy.log` and
`hub-initialization-msrv-1.89.0.log`. The Unix symlink case awaits portable CI.
Native UI adoption and broader
diagnostics remain next, alongside all original remaining milestones.

### Shared native configuration creation and CI fixture review

NINA and ASCOM now create files through the same selector and Rust CLI path.
Attachment and creation share the bounded, hidden helper runner; neither frontend
kills a shared host. Review checked filename retention before dispatch, serialized
admission, blocked replay after lost/malformed/cancelled replies, buffer clearing,
bounded file reads and no equipment activation. Reconciliation only identifies
schema/instance/revision; normal host loading still validates settings. An empty
catalog enables editing while keeping Save disabled. The retained filename is
read-only but selectable/copyable after uncertainty; explicit read restores that
target if an ordinary later Browse selected another file.

All 155 NINA tests pass in `artifacts/hub-native-initialization-nina.log` after final
review, including production creation, existing-data preservation, all three lost
reply cases, pending-operation admission, explicit absence and an actual WPF
creation/read/load/empty-editor flow. Both real net48 bitness fixtures execute
creation and file reconciliation and preserve identity; their shared ASCOM/host
regressions pass in `hub-native-initialization-net48-fixtures.log`. The warnings-denied
net48 build passes. The actual WPF render was inspected and documented as a private
lost-reply fixture, rather than hardware acceptance.

Simulation CI 37465679424 and 37465673033 each pass seven jobs but fail Windows in
different existing pipe fixtures. The PR failure occurs when the partial-frame
deadline closes a pipe before its sender completion runs. That deadline test now
uses a known delivered byte and stalled reader directly, while the real pipe still
proves idle connections remain open. The push failure is a cold handshake timeout
before the queued-cancellation case begins; its server read is now armed directly
instead of through a cold Task.Run queue. Production behavior and the 200-ms/two-
second fixture deadlines are unchanged. New disposable-runner CI must verify these
corrections. CLI eeb9208 CI is still active; broader diagnostics and every original
remaining milestone/acceptance gate remain required.

### Shared output health, wire schema and observed exports

The existing native NINA/ASCOM editor and browser now expose cached output health
using the same host-described parameters and generated reply schema. Review
checked whole-output safety permission outside the visible membership page,
inactive unknown/unsafe state, configured Switch intent without granting live
write permission, independent weather failures, saved identity/revision/cursors,
and preservation of sibling source leases and configuration review. Read failures
clear the current observation; lost/malformed/cancelled/obsolete results invalidate
review and block replay until explicit Reload. Local invalid input sends no RPC.
Exports retain observed public host/source/output data and their separate timestamps,
not editable configuration, credentials or arbitrary backend text. Browser export
now records Reviewed accurately when the configuration review survives a cache read.

Review found that a deserialization schema permitted omission of nullable fields
which the presentation expects. The reply now uses schemars' serialization
contract: these fields are required but can contain null. Regression cases remove
a nested nullable health error and require uncertainty without export/replay.
Reference constraints and their siblings both apply; native comparison uses decoded
strings so equivalent Unicode escapes compare equally. Neither frontend implements
safety policy or invents retry scheduling.

All 168 NINA tests pass with warnings treated as errors, including strict malformed
reply cases, cancellation/admission and actual WPF output paging. Real net48 x86/x64
clients pass with editor/API/export checks; builds have no warnings. Full Rust
hub/Alpaca tests, Clippy, Rust 1.89.0, generated-contract freshness, Node and four
independent schema tests pass in the `artifacts/hub-diagnostic-ui-*` logs. The first
concurrent rebuild met a Windows executable lock while the native fixture was
running; after its confirmed completion the sequential Rust build/tests passed.
No fixture process was killed or deadline increased to work around this.

The actual browser verifies safety unknown/unsafe, Switch pagination, independent
weather errors, preserved Review/Apply state and a downloaded Reviewed diagnostic
snapshot. The browser download-event observation timed out, but its success UI and
saved `Downloads/regain-hub-diagnostics.json` were inspected against the private
fixture's instance/revision. Public source status proves zero leases and no source
connections; console warning/error inventory is empty. Evidence is retained in
`artifacts/hub-output-browser-verification.json` and `hub-output-browser-export.json`.
Actual WPF/browser screenshots are labeled simulation; they do not prove hardware
or interactive vendor acceptance.

Native-creation PR CI 37468454801 passes all eight jobs. Its push CI 37468447863's
first x86 COM response timeout remains unexplained; do not erase that evidence.
Diagnostic API de2691a PR 37471446346 and push 37471435465 pass seven jobs, including
Windows. Intel macOS is cancelled by GitHub's outer 15-minute job limit (confirmed
check annotation). Both logs show about three minutes generating the contract,
5.6 minutes release compilation and about two minutes test compilation; the push
run finishes Rust tests and reaches the device CLI. No failed or hung test is
shown. The portable job budget is now 25 minutes; inner request/test deadlines
remain fixed. Complete new CI is required, alongside actor retry diagnostics,
typed proxies/cameras/coordination and every original remaining acceptance gate.

### Real actor polling diagnostics

Review traced publications through initial/pending connection, in-flight sampling,
cycle completion, Retry-After, partial-pass continuation, explicit refresh,
simulation/command state changes and disconnect/shutdown. The actor now publishes
after scheduling the actual next deadline and updating backoff counters, before
delivering its completed poll event. In-flight observations remain readable from
the cache even while I/O is stalled. Reads add no lease or backend call. The new
fields are observations only; scheduling policy and command replay rules remain
unchanged. Review renamed the cycle counter to `attemptsStarted` because it
includes the in-flight attempt, and retained nullable completion state rather
than falsely reporting a finished cycle during sampling.

Four paused-time actor cases prove dispatch at the observed deadline, exhausted
cycles, Retry-After beyond the backoff cap, unchanged observations during cached
reads, partial-pass identity, suspended unrepresentable deadlines, pending and
stalled initial connections, sampling timeouts, lease release and stopped state.
Native/web replies use the regenerated serialized schema and reject missing,
negative or impossible scheduled waits. Summaries qualify the remaining wait as
belonging to its observation time; actor work can delay it. Exports retain that
typed observation rather than estimating a countdown in the frontend.

Full Rust hub/Alpaca suites, strict Clippy, Rust 1.89.0, generated-contract checks,
Node/four independent schema tests, all 171 warnings-denied NINA tests and actual
net48 x86/x64 clients pass (`artifacts/hub-polling-*.log`). The actual WPF render
was inspected. Browser verification proves a real retry observation/export,
preservation of one independent simulated Switch lease, no console errors and
no physical equipment activation. Evidence is in
`artifacts/hub-polling-browser-verification.json`; cleanup disconnects only client
701, confirms zero source leases/transport, and stops only the fixture's verified
process identities. The temporary browser tab has already closed.

Frontend 96ea4e3 CI runs 37475434423 and 37475427160 now pass seven jobs including
Intel macOS; Windows remains live at this observation. The older COM fixture
timeout is not explained by these successes. The browser fixture also found a
production restriction: explicit empty camera profiles fail HTTP startup with
`No camera slots in settings`, preventing accessory-only publication. Added that
refinement to milestone 2; preserve ordinary new-install camera defaults while
allowing a deliberately empty list. Broader proxies/cameras/coordination and all
original remaining acceptance gates stay required.

### Accessory-only HTTP publication and empty setup

Removed the unconditional nonempty-camera requirement for explicitly persisted
profiles. Missing files still use the ordinary two-camera defaults; no hub setting
silently rewrites an existing installation's camera choices. The same validation
now covers startup and reload, rejecting duplicate IDs before replacing live
settings. The root camera editor handles no selected row, hides its unavailable
settings/tabs/save action, retains accessory navigation and supports adding slot 0.
Periodic status refresh also handles an empty list without an exception.

Three profile cases cover empty/restart/add-first persistence, ordinary defaults,
and invalid/duplicate reload preservation. All hub router fixtures now use empty
profiles; a new API case proves catalog identity, rejection of absent-camera
GET/PUT, first-slot setup, persistence and zero equipment leases. The first test
run incorrectly expected an Alpaca JSON error for the absent slot; production
uses HTTP 404, so the assertion now checks that existing contract. The real HTTP
executable fixture starts and restarts on the same empty list while preserving
its existing shared host and catalog.

All 15 Alpaca unit tests, nine executable tests and 14 router tests pass. Strict
Clippy, Rust 1.89.0, formatting and JavaScript syntax pass
(`artifacts/hub-empty-cameras-*.log`). Actual browser verification proves the empty
state and add/save-first-slot flow, no console errors, no advertised unselected
camera and zero source leases/connections. Evidence is in
`artifacts/hub-empty-camera-browser-verification.json`; the actual screenshot is
documented as simulation. Only the fixture's verified host/publisher were stopped,
and its browser tab was closed. Both 96ea4e3 CI runs now pass all eight jobs;
400a74d polling CI is still live. Original remaining gates, including the earlier
COM timeout investigation and broader proxies/cameras/coordination, stay open.

### Native safety expiry fixture establishes actual HTTP backoff

Polling 400a74d PR Windows job 112321024800 fails one of 171 NINA tests at the
unchanged-generation assertion in
`NativeSafetyExpiresDuringHttpBackoffWhileOtherReadingsRemainAvailable`. All Rust
tests and 89 managed recovery tests pass before it. The preserved log
`artifacts/hub-polling-pr-ci-failure.log` proves a changed generation, not the exact
reason for the transport reset. Do not call this checkpoint green or infer a
production reset bug from that assertion alone.

Review found that the fixture called this backoff without sending Retry-After;
it repeatedly replied 503 at the ordinary short poll cadence. It now returns a
two-second Retry-After, longer than the 1.2-second safe lifetime, and the test
first verifies a completed 503, retained generation, still-safe output, waiting/
retry phase and actual scheduled wait. It then retains stale-state expiry,
generation identity, independent weather availability, elapsed-time bound and
recovery checks. A separate fixture test verifies the header is present only
on failed safety polls. Existing transport-loss safety checks remain intact;
production request deadlines, scheduling and policies did not change.

All 172 warnings-denied NINA tests pass locally
(`artifacts/hub-safety-backoff-nina.log`). Five focused repeats also pass, recorded in
`hub-safety-backoff-repeat-*.log`; new disposable-runner CI is still required.
The earlier empty-camera increment also passes all 171 then-current NINA checks.
Broader typed proxies, camera ownership, coordination and every original
remaining acceptance gate stay required.

### Typed focuser controller over existing source ownership

Reviewed the new controller separately from frontend admission. Construction opens
no device. Connection waits on the actor's watch status before capturing a
generation; strict required capability reads share the same fence. Failed setup,
pending-connection cancellation and operation cancellation queue lease cleanup.
All operations, including concurrent requests from one session, receive distinct
control ownership. Existing actor deadlines, queued-request cancellation and
mutation uncertainty remain authoritative.

The review corrected two capability details before commit. Absolute MaxStep is a
coordinate bound, while MaxIncrement limits travel for one move; relative moves
retain signed distances and reject integer-minimum overflow without splitting or
retrying motion. ETA's existing documented coordinate is micrometres, so its
known 1 µm StepSize is retained; EAF/FC3 lack an optical travel conversion and
remain unsupported. Temperature compensation is not silently changed for Move.
Optional errors retain their source code, including unsupported ETA Halt.

`cargo test -p regain-hub -p regain-alpaca --locked` passes with production workers
provided through REGAIN_TEST_WORKERS. Thirteen new actor/actual HTTP cases cover
independent leases, live limits, malformed capabilities, optional errors,
asynchronous readiness/deadline, generation replacement, control conflicts,
preflight cancellation, dispatched cancellation/uncertainty and no replay. The
native suite now has eight cases, including the typed controller using three
production workers in explicit simulation. Final focused checks pass after the
ETA adjustment, as do strict Clippy, Rust 1.89 and generated-contract freshness
checks. Logs: `artifacts/hub-focuser-{rust,integration,clippy,msrv,contract}.log`.

The controller is not yet a published hub output. Current runtime still rejects
Proxy and does not advertise proxyOutputs; no generated contract or frontend
claim changes here. Next is runtime/IPC/typed diagnostics plus publication and
imports. Conformance, hardware acceptance and the full original milestones remain
required. Empty-profile push 37479602454 passes all eight CI jobs. Safety fixture
correction 37480604743/37480593803 each pass seven jobs with Windows build/package
still live at this checkpoint; do not call those runs fully green yet.

### Focuser runtime, private IPC and observed typed diagnostics

Runtime admission now accepts native/Alpaca focuser proxies, retaining independent
output sessions over one source actor. Unsupported proxy classes and unsupported
focuser source adapters fail explicitly. Pending sessions participate in apply
admission; cancelled connection/EOF releases only that client's leases. Private
IPC has typed property enums and strict command fields. It preserves the same
control/generation checks as direct controller calls and rejects class mismatch.
After source reset, Connected is false for the old session and commands cannot
adopt its replacement; explicit disconnect/connect is required.

Reviewed cache generation races: DeviceState rechecks the captured snapshot's
generation and projects only known IsMoving/Position/Temperature observations.
Neither DeviceState nor paged diagnostics starts I/O. Typed samples retain local
ages and source identity/generation/revision/sequence; relative focusers cannot
invent an absolute cached coordinate. Poll plans deduplicate all nine properties,
using strict boolean requests and numeric requests with typed decoding. Native
workers provide their known constants and explicit optional-property errors.
Diagnostics use generated schema and host-described order/types/ranges. Both web
and native readers reject wrong property/source/generation/sequence/type/range,
negative ages and unexpected fields. Their summaries/exports retain observation
ages and uncertainty without permitting a command.

Full hub/Alpaca suites, strict Clippy, Rust 1.89 and generated-contract freshness
pass. The final focuser suite has 18 cases and the native suite nine, including
actual HTTP incremental polling, private duplex IPC and a production-worker
runtime in explicit simulation. Node and four independent schema tests pass.
All 173 warnings-denied NINA checks pass; the final typed fixture refinement
also passes independently. Warning-denied net48 build and real x86/x64 client
fixtures pass. Logs are `artifacts/hub-focuser-runtime-*.log`. Initial concurrent
Rust/.NET checks hit Windows executable replacement denial; serialized checks
pass. The HTTP fixture now waits for both required partial samples instead of
assuming Position arrival means IsMoving has also arrived. Deadlines unchanged.

Both safety-correction runs 37480604743/37480593803 pass all eight CI jobs.
Focuser-controller runs 37483351644/37483341547 each pass seven with Windows build/
packaging still active. New CI remains required. Setup does not advertise general
proxy support, and the existing Alpaca catalog explicitly rejects classes whose
routes are not implemented. Alpaca/NINA/ASCOM focuser publication, imports,
conformance and every original remaining milestone remain required.

### Alpaca focuser publication (2026-10-06)

Reviewed publication against the [ASCOM Focuser V4 contract](https://ascom-standards.org/newdocs/focuser.html)
and the shared controller/IPC implementation. HTTP maps all nine property names
through the Rust enum instead of duplicating typed definitions. Move parses a
signed Int32 and delegates live motion/range/generation checks to the controller;
Halt and TempComp use the same command ownership. V4 advertisement requires the
host's modern connection/DeviceState capabilities, while the catalog requires
`focuserOutputs`. Optional unsupported readings retain a standard error and
sanitized text. COM-only Link is not published as an Alpaca member.

Review identified class-local number collisions between existing local slots and
hub outputs. A shared server catalog check rejects collisions, including reserved
unconfigured slots, before dispatch or setup selection. UUIDs/numbers are never
silently rewritten. Distinct local slots retain their routes and setup pages;
hub focuser setup opens the existing shared editor. Diagnostics and DeviceState
remain cached and cannot initiate motion. The general proxy setup capability
stays gated pending native frontend publication.

Four private loopback HTTP cases pass through the production source adapter,
shared host, IPC and router. They cover sparse identities, two independent sources,
two outputs sharing one source, legacy/asynchronous connections, independent
ClientID leases, absolute travel/relative signed limits, busy motion, strict
types and duplicate parameters, optional errors, cached DeviceState, local slot
coexistence/collisions and retained uncertainty after an acknowledged move's reply
is lost. That move is dispatched exactly once; another client cannot replay it,
and Connected=true cannot silently adopt a replacement generation. No physical
equipment is activated. Review added the explicit non-colliding local-slot case.

Full local Rust hub/Alpaca suites pass; the final four focused cases, strict Clippy,
Rust 1.89.0 and formatting/diff checks pass. Logs are
`artifacts/hub-focuser-alpaca-{rust,focused,clippy,msrv}.log`. These changes do not
alter native frontend code or generated configuration. Controller push CI
37483341547 passes all eight jobs; its PR run 37483351644 was subsequently
cancelled. Runtime/IPC PR 37486781939 and push 37486773101 each pass seven jobs
with Windows still live. Native NINA/ASCOM publication, imports/simulation,
conformance and all original remaining milestone gates remain open.

### Native focuser outputs (2026-10-06)

Added a shared typed focuser request/value contract in the existing common .NET
assembly, native NINA provider/device and ASCOM V4 adapter with V3/V2 QI. Attachment
requires focuser/modern-connection/DeviceState capabilities before leasing
equipment. Selection enumeration remains file-only; the common themed selector,
manager and registration backend now admit Focuser with a stable 39-character
`Rgn.HF.<uuid>` ProgID and both Chooser views. ASCOM returns after Move starts and
uses explicit Halt; Position retains Int32 in DeviceState. Optional-property
errors and native ASCOM translation reuse the existing implementation.

NINA's interface requires absolute Position. Preparation rejects relative sources
without inventing a coordinate or actuating a move. NINA waits for motion to stop,
checks the exact target, then uses the requested settling delay. Failed IsMoving
reads are errors, never fabricated idle states. Unsupported optional readings
become NaN without falsely reporting a source failure. Cancellation does not
automatically Halt a source that another client may now control. Source-generation
checks and uncertain writes remain in the Rust controller; native Connected reads
the actual generation-bound output state. Longer-lived coordinated ownership
remains an original unfinished gate.

Reviewed binding/capability admission, request epoch capture, disconnect/Dispose,
no automatic replay, shared source uncertainty, integer state conversion and
registration/export factory mapping. The private loopback fixture is shared by
net8 and net48 tests and joins owned request tasks before teardown. Review
strengthened cancellation to require OperationCanceledException and added private
Focuser Chooser registration/removal checks. The initial net48 fixture used LINQ
on a COM collection without IEnumerable; indexed COM access fixes it without
changing a production deadline.

All 180 warnings-denied NINA tests pass, including malformed motion reads and cancellation.
Real net48 x86/x64 clients prove current/legacy QI, absolute/relative motion,
typed state, independent leases, Link/asynchronous connection and lost-reply
fencing. Warning-denied NINA/ASCOM builds and both staged worker bitnesses pass.
Actual manual COM exports pass with five outputs and both client/server bitnesses,
including metadata without host activation and Focuser properties/Move/Halt/TempComp.
Logs are `artifacts/hub-focuser-frontends-*.log` and
`artifacts/hub-focuser-nina-focused.log`. Local HKCU SCM activation fails at the
first existing Switch class with REGDB_E_CLASSNOTREG; its cause remains unproved,
and the fixture removes all private registrations/processes. The updated SCM and
production-registration cases must run in disposable Windows CI. No installed
vendor driver or physical equipment is activated.

Runtime/IPC PR CI 37486781939 passes all eight jobs; its push run 37486773101 is
cancelled after seven successes. Alpaca publication PR 37488895245 passes seven
with Windows still live. New CI remains required. Typed focuser COM/virtual/
dedicated simulation imports, general typed setup, conformance, interactive and
vendor/hardware acceptance and every original remaining milestone remain open.

### Windows COM focuser imports (2026-10-06)

Extended the existing isolated STA import worker and Rust factory to focusers.
The shared focuser controller, sampling plans, leases and generation fences are
reused. Reads cover all nine typed properties; integer positions and limits retain
Int32. Only Move(Position), Halt and TempComp are writable. Strict parameter and
return-value validation occurs before dispatch or publication; errors remain
sanitized. Relative Position stays unsupported. The existing uncertain-write latch
prevents subsequent mutations, replay and automatic Halt after unknown completion.

Reviewed interface version negotiation, managed/external ownership, required and
optional property shapes, class admission, source sharing and cleanup. Focuser's
asynchronous interface begins at V4. The initial actual parent test exposed a
second version threshold in Rust that still expected asynchronous connection at
V3; both sides now agree, without increasing deadlines. The test initially used a
nonexistent fixture helper and ignored shutdown's Result; both are corrected.

`scripts/test-hub-com.ps1` passes warning-denied x86/x64 worker/fixture builds,
20 worker tests (including V3/V4 in both architectures) and 12 actual registered
Rust parent cases. The new parent case proves shared activation/leases, limits
above Int16, live motion/control, last-client retention and one dispatched
uncertain vendor Move without replay. Full local Rust hub/Alpaca suites, strict
Clippy, Rust 1.89 all-target checks, 180 warning-denied NINA tests and real net48
x86/x64 frontend fixtures pass. Logs use `artifacts/hub-focuser-com-*.log`.
COM parent tests require the private-registration harness; ordinary cargo runs
without its environment do not prove those cases. Physical equipment is untouched.

Native frontend PR CI 37492070586 and push CI 37492059703 each pass seven jobs,
but Windows fails production registration before activation with KeyError:
focuser. Both logs first prove actual cold SCM launch, five output classes and both
client/server architectures. The production-registration fixture omitted Focuser
from its Chooser-path collision/cleanup mapping; that mapping is corrected. Python
syntax and diff checks pass. The actual machine-registration check requires new
disposable Windows CI; local machine-fixture guards remain intact. Failure logs
are `artifacts/hub-focuser-frontends-{pr,push}-ci-failure.log`.

Next: dedicated typed simulation and virtual focuser inputs, general typed setup
and remaining accessory proxies. Camera buffers/acquisition, coordination,
conformance, resume/recovery, interactive/vendor/hardware acceptance, main
reconciliation, documentation/site/screenshots and all original final gates remain
required before merging PR21.

### Virtual focuser composition (2026-10-06)

Extended the existing virtual backend to typed focuser reads and Move/Halt/TempComp
dispatch through the shared controller. Strict signed Int32 and boolean parameters,
live capability/motion preflight, optional errors, leases and shared uncertainty are
preserved. Polling uses a generation-checked cached sample accessor; boolean/integer
values retain their types, missing/invalid properties retain errors and sample ages
are propagated rather than refreshed by virtual polling. An invalid inner focuser
session retires the virtual transport, fencing the old outer session. Disconnect
does not Halt and uncertain writes do not replay.

Review found that awaiting a complete inner focuser connection in Backend::connect
would consume the outer request deadline. Focuser connect_step now starts one
supervised inner operation and observes readiness in bounded steps. Existing scalar
virtual connection semantics remain unchanged. Cancellation closes the private
inner client and releases pending leases; existing graph validation and transitive
simulation marking are reused.

Seven private loopback tests cover two nested layers and a concurrent leaf client,
Int32 positions, absolute travel/busy limits, signed relative moves with unsupported
Position, optional properties, preserved ages/errors, a handshake longer than the
outer request step with exactly one upstream version probe/open, cancelled pending
connection cleanup, invalid motion reads before dispatch, and one uncertain Move
with no replay/automatic Halt/generation adoption. Existing nine scalar composition
cases also pass. Full Rust hub/Alpaca suites with production workers in explicit
simulation, strict Clippy and Rust 1.89 all-target checks pass. Logs use
`artifacts/hub-focuser-virtual-*.log`; no physical equipment is activated.
An eighth test uses the production EAF worker with explicit simulation and proves
motion completion and transitive simulation marking through both virtual layers,
with no loopback fallback. It requires REGAIN_TEST_WORKERS and was run with that
environment. All 180 warnings-denied NINA regressions and real net48 x86/x64 clients
pass against the rebuilt host. These regression fixtures do not establish new
interactive or vendor/hardware acceptance.

The initial fixture omitted managed connection policy; the backend correctly
refused to connect its disconnected externally managed mock. It now explicitly
owns that private connection. A later malformed-motion assertion expected the
live controller's Unavailable classification in the cache; the sampling adapter
classifies malformed Alpaca wire values as Permanent, and virtual samples correctly
preserve that error. The assertion is corrected without changing production
classification or increasing any deadline.

COM import/registration checkpoint 8c806d5 CI 37494625707 and 37494616586 remain
active. Dedicated focuser simulation, shared typed setup, other device classes,
cameras/acquisition, coordination and all original acceptance/final gates remain
required. Current native/web source-choice gates are not evidence of completed
typed setup; no general proxy capability is enabled by this increment.

### Dedicated focuser simulation (2026-10-06)

Added typed Focuser V4 simulation to the existing source actor. The same controller,
leases, sampling and generation fences serve every frontend. Fifteen controls
(thirteen state fields, fault and sample age) derive from Rust defaults/descriptors;
native and web readers enforce strict Int32, numeric bounds, nested status members
and sparse updates. Invalid patches validate a private candidate and leave current
state unchanged. Existing scalar status shapes omit the new optional focuser field.

Reviewed class admission, overflow, optional properties, relative coordinates,
monotonic completion, teardown, fault mutation and uncertainty. Move acknowledges
start; disconnect does not Halt and retained timed motion can complete without a
lease. Relative moves never fabricate Position. Stalls, stopped-short completion
and malformed IsMoving are explicit faults. Applying a write before an uncertain
reply invalidates connected sessions and retains the actor's latch; clearing the
injected fault does not replay, reconnect or clear it. Injecting coordinates,
limits or motion replaces pending test motion; unrelated updates do not halt it.

Six new Rust cases bring the simulation suite to fifteen. Full local hub/Alpaca
suites (with production workers explicitly simulated), strict Clippy, Rust 1.89
all-target checks, generated-contract freshness, Node and four independent schema
checks pass. All 184 warning-denied NINA tests pass, including four new focuser
simulation cases. Real net48 x86/x64 adapters prove shared integer controls, timed
motion, compensation, optional errors and lease cleanup. Logs are
`artifacts/hub-focuser-simulation-*.log`. The actual WPF capture is in the development
setup guide and explicitly labelled simulation; new browser rendering still needs
acceptance. No physical equipment or installed vendor driver was activated.

Test review corrected an assertion that ignored source-generation invalidation
following uncertainty; it now requires Disconnected and observes the outcome only
through explicit simulation status. Temporary update leases are released
asynchronously; cleanup assertions wait for that release instead of assuming it
preceded the reply. The WPF capture waits for layout before scrolling to the changed
integer field. Production policies and deadlines were not loosened.

COM checkpoint 8c806d5 push CI 37494616586 passes all eight jobs, including actual
worker/parent fixtures, cold SCM activation, production registration and installer
checks. Its PR run 37494625707 was cancelled after seven successes. This closes the
missing Focuser Chooser-path fixture correction, not the earlier unrelated COM
response timeout or local HKCU SCM investigation. Virtual checkpoint 1af147b runs
37496570309/37496563543 remain live at this observation. Next: shared typed setup,
remaining accessories, camera acquisition/buffers, coordination and every original
acceptance/final gate. PR21 remains draft.

### Shared focuser configuration setup (2026-10-06)

Enabled proxy creation through shared descriptors with explicit per-class gates.
The host advertises proxyOutputs/focuserOutputs; only Focuser can be selected.
Camera and other unfinished proxy classes require unadvertised broaderProxyOutputs.
COM class choices now include the implemented Focuser import, while transport and
bitness admission still depend on actual staged workers. No frontend device list
or second schema was added. Initial proxy values select Focuser instead of Camera.

Reviewed metadata/runtime agreement, default generation, stable IDs/numbers,
source matching and no-I/O configuration semantics. Actual native editor creation
adds one explicit simulator and two outputs, rejects an unsupported camera candidate
in host Review, applies/reloads without equipment leases and preserves both IDs.
The two NINA clients then share position/motion and release independent leases.
The net48 fixture verifies the same host choices/defaults on both architectures.
All 186 warning-denied NINA tests and real net48 x86/x64 suites pass. Full local
hub/Alpaca suites with explicitly simulated production workers, Clippy and Rust 1.89
checks pass; logs use artifacts/hub-focuser-setup-*.log.

Actual browser acceptance found an existing tagged-choice closure reading the
schema variable after rendering had replaced it with the selected variant. Changing
source transport or output kind threw a TypeError. The handler now captures the
original described choice list and refuses unavailable choices. A small DOM adapter
runs the actual renderer/event handlers to regress transport changes, conditional
field replacement and Focuser-only proxy defaults; it is not evidence of layout.
Chrome acceptance separately proves simulator/output creation, review/apply/reload,
rejected fractional Position, a sparse Position 100000 update preserving temperature
12, unchanged revision, zero source leases and no console errors. Actual screenshots
are in the development setup guide. In-app browser input timed out; Chrome completed
the flow against the same private server. A default Python lacked jsonschema; the
independent schema checks now pass all four cases in a private test environment.
Generated-contract freshness, formatting and diff checks also pass.

Automatic approval review rejected a combined background fixture launch with
blocked by policy. The test used an inspectable foreground session instead. All
private publisher/host processes were identified by their unique executable and
configuration paths and stopped after verification. No physical equipment or
installed vendor driver was activated. Virtual checkpoint push CI 37496563543
passes all eight jobs; PR 37496570309 was cancelled. Simulation checkpoint runs
37499571887/37499559138 remain live at this observation. All original camera,
coordination, remaining typed interfaces, conformance, interactive/vendor/hardware,
resume/recovery, discovery, main reconciliation and documentation/final gates remain
required before PR21 can merge.

### Typed rotator controller and shared accessory sessions (2026-10-06)

Extracted only connection readiness, immutable generation checks and unique
command admission from Focuser into TypedSourceSession. Reviewed the extraction
against the existing implementation: the enclosing whole-handshake deadline still
includes capability reads, uncertainty is checked before adopting any generation,
and cancellation/drop releases only owned leases through the same actor FIFO.
Device-specific limits, motion and optional properties remain in their controllers.
All eighteen existing focuser actor/transport/runtime/IPC cases pass after extraction.

The new rotator controller validates seven typed properties, keeps logical,
mechanical and target angles distinct, and forwards exact signed relative angles.
Absolute/mechanical/reference commands validate their range; malformed motion is
not idle. Per-operation ownership arbitrates even simultaneous calls on one
session. Live reversal capability and optional Halt/StepSize errors survive.
Move acknowledges start; Sync does not synthesize physical motion or a private
offset. Sync/Reverse require idle through explicit hub preflight. Native source
reference persistence must be proved before publication; modern interface reversal
requirements and older-source capability admission remain part of that work.

Eleven tests cover shared leases, connection/capability cancellation, limits,
malformed readings, optional errors, preflight generation loss, dispatched
uncertainty and actual Alpaca V3/V4 transport. The HTTP fixtures verify exact
Position/Reverse parameters, shared ClientID, unique transactions, connection
version negotiation and one final owned cleanup. An applied move with malformed
acknowledgment latches uncertainty without replay or automatic Halt. Initial test
compilation fixes used the existing Backend reset/SourceError fields, actual
connection_info snapshot field and cancellation error access; no production API
or deadline was altered to accommodate a fixture.

Full hub/Alpaca suites pass with explicitly simulated production workers. Strict
Clippy, Rust 1.89 all-target checks, generated-contract freshness, Node and four
independent schema cases pass. All 186 warning-denied NINA tests and real net48
x86/x64 clients pass against the rebuilt host. Logs use artifacts/hub-rotator-*.log.
The first freshness invocation named a nonexistent export_description example;
the actual CI export_config command was then run and passed. No physical equipment
or installed vendor driver was activated. Runtime/IPC, native adapter persistence,
COM/virtual/simulated imports, frontends and setup still require implementation.

Simulation checkpoint PR/push CI 37499571887/37499559138 now passes all eight jobs.
Shared setup push 37501487430 fails an initial connection in the NINA uncertain-
Move fixture, before any injected move. Its generic exception hides the structured
error; this does not establish a timeout or reconnect cause. The fixture now
reports first/second connection stage, structured code/message, actual source
snapshot and private request start/reply/close timings on failure. It retains the
same deadlines and does not retry. The failed log is preserved in
artifacts/hub-focuser-setup-push-ci-failure.log. Local success cannot close this
investigation. PR 37501496852 remains live at this observation. Preserve the older
COM response-timeout investigation and all original acceptance/final gates.

Ten separate local runs of the affected cold connection/uncertain-Move test pass
without retries inside the test. This does not reproduce or explain the CI failure;
the new evidence must be inspected if it recurs. Logs use
artifacts/hub-focuser-connection-audit-1.log through -10.log.

### Native rotator references and typed adapters (2026-10-06)

CAA/Falcon adapters now expose CanReverse, Reverse and StepSize alongside separate
logical/mechanical/target angles. Sync and Reverse use private durable reference
records, an Uncertain marker before worker dispatch, strict accepted/readback
confirmation and a revision-checked known commit. Reviewed marker/save failure,
source recreation, corruption, identity/simulation separation, cancellation and
cross-store races. Blocking filesystem work runs off the async executor; the OS
lock and revision comparison prevent a late save from clearing a newer marker.
No storage means reference writes fail before dispatch. Runtime binds the store
to its private endpoint; an account without a usable data directory can still
host other sources but cannot silently use transient native references.

Reconnect checks actual direction and restores only worker-local offset/target.
It does not move, change direction or reset the mechanical origin. A direction
mismatch makes logical coordinates unavailable until explicit Sync. Unknown
references still permit mechanical movement and Halt. Native relative commands
retain their existing +/-360 degree limit. Final vendor-worker retirement/fault
cleanup can attempt a stop; dropping one shared controller lease does not inject
Halt or retire a worker still owned by another client.

Five Windows storage tests pass: independent bindings, cancelled late commit,
competing stores, malformed/oversized/foreign records and invalid inputs. A sixth
Unix permissions/symlink case requires portable CI. All thirteen native cases
pass with production workers in explicit simulation, including reference recovery
and private inert-worker crashes after applied Sync. The fixture also returns
accepted without applying Sync: readback retains uncertainty rather than saving
a guessed offset. Both failure modes survive complete source/worker recreation;
trace assertions prove no automatic Sync replay, direction or origin write.

The new CAA local-restore test first failed with logical 346.5 instead of 42.5.
Review found settings observed an external Reverse change without updating the
cached direction unless the driver itself had initiated it. Settings now updates
the direction; the hub samples settings before logical status and rejects a
changed reference. Sync also updates CAA TargetPosition. Eight CAA controller,
fourteen CAA protocol and eight Falcon tests pass, including wire-level proof
that restoring offsets does not issue movement/direction/origin writes.

After rebuilding both actual executables, full hub/Alpaca regressions, strict
Clippy across hub/Alpaca/ZWO/Pegasus, Rust 1.89 all-target checks, generated-contract
freshness, Node/four independent schema checks, all 186 warnings-denied NINA tests
and real net48 x86/x64 clients pass. Evidence uses artifacts/hub-rotator-native-*.log;
the final regression set uses the -final- prefix. No equipment or installed vendor
driver was activated. Rotator runtime/IPC, imports, frontend publication, shared
setup, conformance and all original remaining gates are still required.

Shared-setup PR CI 37501496852 and controller PR CI 37503876752 pass all eight
jobs. Controller push 37503869679 was cancelled after seven successes. Preserve
the unexplained earlier initial-connection failure and COM timeout; later green
runs do not establish their causes. This newer increment requires new CI.

### Rotator runtime, typed IPC and cached diagnostics (2026-10-06)

Admitted native/Alpaca rotator outputs through the existing runtime, private client
leases, pending connection admission and immutable typed sessions. Reviewed all
six explicit command dispatches, wrong-class/argument rejection, cancellation,
generation loss, concurrent ownership, IPC EOF and no replay. Connected now uses
one common typed-session accessor for both focusers and rotators. Frontend setup
capabilities still enable only Focuser; the IPC capability does not promise a
completed NINA/ASCOM/Alpaca interface or completed COM/virtual/simulated input.

Seven properties deduplicate into the poll plan. Review found typed insertion
could bypass the combined sample bound after scalar mappings; the final limit
now covers all classes. The factory boundary test reaches exactly 1024 samples,
then rejects 1025 with unchanged configuration for both focusers and rotators.
Initial fixture compilation needed explicit error extraction because SourcePlan
has no Debug implementation. Scalar property fixtures require lowercase letters;
their initial numeric names were corrected without relaxing production validation.

Cached rotator samples preserve strict types/angles, optional upstream errors,
monotonic ages and source/revision/generation/sequence. Diagnostics stay inert and
paged; failed properties do not erase unrelated mechanical readings. Both native
and web readers use the generated response schema and common typed accessory
validation, with host-described minimum/exclusive maximum/Single-range limits.
Native tests cover identity, type, range, sequence and age faults; web tests cover
eleven malformed reply cases. Existing summary rendering is reused; broader
interactive acceptance and rotator frontend publication remain required.

Review against [Rotator V4](https://ascom-standards.org/newdocs/rotator.html) and
the [read-all rules](https://ascom-standards.org/newdocs/readall-faq.html) removed
Reverse/TargetPosition from DeviceState. Only available IsMoving, MechanicalPosition
and Position belong there; richer observations remain in diagnostics. No query
timestamp masquerades as a measurement. Tests assert the exact names and omit
invalid motion/position while preserving a valid mechanical position.

Fifteen rotator cases now include four runtime/IPC cases and three actual V3/V4
loopback transports built through the real configuration-derived source factory.
They exercise sparse identities, shared leases, every command, cancellation,
read-generation loss, EOF and applied malformed replies. Initial integration
fixtures used the private OutputConnection get method; they now observe actual
IPC instead of widening production access. The EOF fixture initially expected
an actor latch after final teardown; inspection of the existing source/focuser
contract corrected that assertion. It verifies retained uncertainty before EOF,
no replay and final lease cleanup; native durable reference markers are separate.

Fourteen native cases pass with actual production workers in explicit simulation,
including the new runtime/factory path for both CAA and Falcon, shared sources,
Sync/target confirmation and cached health. No physical equipment or installed
vendor driver is activated. After rebuilding actual workers/host, full Rust
hub/Alpaca suites, strict Clippy, Rust 1.89 all-target checks, generated-contract
freshness, Node/four independent schema tests, all 187 warnings-denied NINA tests
and real net48 x86/x64 clients pass. Evidence uses
artifacts/hub-rotator-runtime-final-*.log; focused tests use
artifacts/hub-rotator-runtime-focused.log. Native-reference PR CI 37508673278
has seven successes and Windows still running; push 37508667983 also remains live
at this observation. This newer runtime increment needs its own CI. All original
remaining milestone and acceptance gates stay required before PR #21 can merge.

### Alpaca rotator publication and modern readiness (2026-10-06)

Reviewed the existing publisher/router rather than introducing another host or
connection owner. Rotator catalog entries require the typed IPC capability and
retain saved UUIDs/sparse numbers. The existing independent ClientID sessions,
asynchronous progress, immutable source generations, uncertainty fences and
client capacity bounds remain in use. Every typed property/command maps through
the common IPC; the hub preserves signed relative distance and delegates source
coordinates/reference operations. Local slots coexist at distinct numbers.
Collisions reject reads, connection/movement writes, catalog and setup before
opening a source. Setup serves the common editor without falsely enabling
unfinished rotator creation. Wrong class/member/casing and malformed/range-invalid
parameters cannot dispatch. DeviceState preserves valid mechanical data while
omitting invalid logical/motion readings; it neither invents success nor a timestamp.

Review against [IRotator V4](https://ascom-standards.org/newdocs/rotator.html)
found that modern reversal support is required, while StepSize remains optional.
Modern runtime admission therefore checks CanReverse=true and a strict Reverse
reading inside the existing whole-handshake deadline. A paused-time test uses
a 30-second source request and proves failure at the two-second connection deadline,
then eventual lease cleanup behind the bounded actor read. No production deadline
was increased. Generic controller capability inspection still supports legacy
sources. Unsupported/missing reversal cannot be advertised as connected. Sync and
async HTTP fixtures check the retained failure and fixed Regain explanation;
arbitrary backend strings remain excluded by the existing sanitization policy.

Shared private HTTP fixtures now parameterize accessory type/version instead of
duplicating the focuser upstream. Seven rotator cases cover V3/V4 negotiation,
dynamic identity, two shared outputs plus an independent source, all six commands,
busy admission, optional/malformed data, cache, local coexistence/collision and
an applied lost-reply move without replay or per-client automatic Halt. One uses
the actual CAA and Falcon executables in explicit simulation through the complete
factory/host/IPC/publisher/router path. It verifies acknowledged motion completion,
shared ownership and saved logical reference after complete worker recreation.
No physical device or installed vendor driver is activated.

The first focused run passed 20 of 23 HTTP cases. Three assertions were wrong:
the page loads hub.mjs, and unavailable values/admission already map to 0x402,
not 0x500. The fixtures now assert those actual contracts without weakening
production errors or deadlines. Review also strengthened catalog length, valid
mechanical cache preservation and mutation rejection on a colliding slot.
The expanded focused run passes all seven rotator HTTP cases; the hub rotator
suite passes sixteen cases including modern readiness. Final evidence and CI
status are recorded in the plan. Native NINA/ASCOM rotators, COM/virtual/simulation
imports, shared creation and all original broader acceptance gates remain open.

Final rebuilt-worker/host Rust hub/Alpaca suites, strict Clippy, Rust 1.89 all-target
checks, generated-contract freshness, Node/four schema checks, all 187 warnings-
denied NINA tests and real net48 x86/x64 clients pass. Evidence uses
artifacts/hub-rotator-alpaca-final-*.log. Reference PR/push CI
37508673278/37508667983 now passes all eight jobs. Runtime PR CI 37510990909 has
seven successes with Windows still running; push 37510983650 also remains live.
Neither later passing tests nor pending jobs explain the retained earlier COM
and NINA initial-connection failures. This Alpaca increment requires new CI.

### Native NINA/ASCOM rotators and relative completion (2026-10-06)

Reviewed publication through the existing shared native session and COM export
server. Rotators reuse saved identities, the themed selector, isolated host
attachment and shared source/control ownership. Common typed NINA request handling
now serves focusers and rotators. Strict rotator keys/value validation is shared
between NINA and ASCOM. Private HTTP accessory fixtures also share their framing
and connection machinery rather than duplicating the focuser fixture.

NINA requires the receipt capability before connecting equipment. Review found
that reading TargetPosition after releasing command control could observe a
sibling's Sync, and accepting only the reported target could certify an ignored
relative move. The host now holds control across pre-position, signed dispatch,
ACK and target readback; the receipt includes expected and accepted targets.
An actual two-client IPC fixture pauses that read, proves the control lease is
retained and verifies sibling writes cannot replace it. Failed readback after
an unambiguous ACK remains unavailable, not a fabricated uncertain-write latch.
NINA verifies receipt agreement and actual completion with circular error and
resolution tolerance. Cancellation/stopped-short/ignored/lost-reply cases never
replay or implicitly Halt. Optional StepSize is read after mutation admission to
preserve uncertainty priority. Per-connection Synced remains only an indication
of successful Sync for that epoch; source coordinate mapping is shared.

Review corrected Sync indicator lock ordering, Single rounding at 360 degrees,
underflowed positive StepSize, ASCOM nonfinite-command exception classification
and standard DeviceState boxing as Single. ASCOM V4/V3/V2 moves retain their
acknowledged-start contract. Stable UUID-derived Rotator registration is covered
alongside Focuser registration. Six private exported outputs now exercise rotator
metadata, both server architectures and both client bitnesses. No installed vendor
driver or physical equipment is activated.

Final full Rust hub/Alpaca suites pass, including seventeen rotator cases, rebuilt
production-worker simulation and endpoint process fixtures. Strict Clippy,
Rust 1.89 all-target checks, contract freshness, Node/four schema checks, all 196
warnings-denied NINA tests, real net48 x86/x64 clients, both-architecture staging
and manual private COM exports pass. Evidence uses
artifacts/hub-rotator-frontends-final-*.log. The first parallel Rust compilation
failed with Windows OS1455 (paging-file exhaustion); the retained log is
artifacts/hub-rotator-frontends-final-rust.log. Serial build retry passed without
changing tests. A test MutexGuard's explicit drop still triggered Clippy;
lexical scope now ends the guard before await.

Alpaca checkpoint PR/push runs 37513525462/37513518205 ended cancelled after seven
successful jobs. The Windows check annotation explicitly says the job exceeded
25 minutes. Its retained log, artifacts/hub-rotator-alpaca-cancelled-windows.log,
shows builds, tests, packaging and installer uploads completed before cancellation
during standalone camera-kit dependency installation. The outer Windows workflow
budget is now 45 minutes; device, operation and test deadlines are unchanged.
Runtime runs 37510990909/37510983650 also ended cancelled; their exact cancellation
cause has not been independently established here. Reference runs
37508673278/37508667983 passed all eight jobs. New CI, including registered SCM
exports on disposable runners, remains required. Neither this timeout finding nor
passing local checks explains the retained older COM/NINA connection failures.

Rotator COM/virtual/dedicated simulation inputs, shared creation, interactive
acceptance and conformance remain open. Wheels, panels, camera ownership/transport,
coordination and all other original plan gates remain required before merge.

### Virtual rotator composition (2026-10-06)

Extended the existing virtual accessory transport rather than adding another
host or controller. Focuser and rotator inputs now share supervised, bounded inner
admission and immutable-generation checks. Rotator reads/commands still go through
the inner typed controller, including all six mutations, signed relative distance,
separate source-owned coordinates, modern reversal readiness and optional errors.
Strict parameter shapes cannot select another command or supply a guessed offset.
The cached typed sampling path shares age/error forwarding and batch insertion;
the rotator session exposes its existing generation-fenced cached decoder.

Five new real factory/loopback cases extend the existing V3/V4 transport fixture
through two virtual graph levels. They cover all commands, independent clients,
optional StepSize, malformed motion, Busy, negative -721.5-degree dispatch,
source-coordinate sharing and complete lease cleanup. A 700-ms leaf handshake
outlasts each virtual source's 100-ms request step without repeated interface
negotiation. Faster outer polling preserves increasing age while the leaf sequence
is unchanged. Pending cancellation releases supervised inner clients without a
move/Halt; generation loss in preflight cannot dispatch and old sessions cannot
adopt a freshly connected generation. Applied malformed move acknowledgment is
dispatched once and fences siblings without replay or implicit Halt.

Native-worker coverage reuses the existing runtime fixture for both CAA and
Falcon in explicit simulation. Direct and nested clients observe the same verified
Sync/target reference and cached health; closing one retains the other's source
lease. Simulation labels propagate to every nested output/source. No physical
equipment or installed vendor driver is activated. This establishes native graph
integration, not dedicated rotator simulation or hardware acceptance.

Fixture review corrected an invalid 50-ms poll interval to the existing 100-ms
minimum, retained the saved outer UUID instead of exposing private configuration,
and added the existing cached decoder accessor. An outer cached mechanical reading
can arrive before the optional-property error; the test now waits for both actual
observations and still requires Unsupported with no invented StepSize. Production
timings, error classifications and validation were not relaxed.

Full rebuilt-host Rust hub/Alpaca suites pass, including twenty-two rotator and
fifteen native cases plus existing virtual focuser/scalar regression suites.
Strict Clippy, Rust 1.89 all-target checks, generated-contract freshness, Node
and four schema tests pass. Evidence uses artifacts/hub-virtual-rotator-final-*.log.
An initial host rebuild hit OS5 while a private managed fixture owned the
executable. That fixture completed (196 NINA tests and net48 clients passed);
the sequential rebuild then succeeded. Those earlier managed checks used the
previous executable and cannot certify the rebuilt-host checkpoint.

The rebuilt-host NINA run ended with 191 passes and five failures:
failures: ActualAlpacaPublisherAndNativeNinaShareSwitchStateAndSeparateLeases
(cancelled output read during cleanup), NativeEditorWindowEditsReviewsAppliesAndShowsSavedHealth
(attachment closed), and UnknownCreationRetainsFilenameAndReadsCommittedFileWithoutReplay
for malformed completion (read reported Missing rather than Existing),
SharedNativeSelectorRetainsUnknownFilenameAndEnablesLoadingOnlyAfterRead (UI wait),
and NativeCreationUsesProductionPersistenceAndPreservesExistingData (helper deadline). Preserve
artifacts/hub-virtual-rotator-final-rebuilt-nina.log; their causes remain unproved.
The seven-case focused run passed six and reproduced publisher readiness timeout
before connection. The other later passes do not establish their earlier causes.
Rebuilt-host net48 checks did not run because the sequence stopped at NINA failure.
No production timeout or test requirement has been relaxed.

Publisher fixture review found it used the installed user profile path. It now
supplies a private persisted empty camera list and --simulate, and asserts exactly
three scalar hub catalog entries with no cameras. This removes user-profile
migration/discovery from the fixture independently of the readiness failure.
Cleanup no longer masks an earlier assertion with a cancelled stdout read, and
prints private captured output on failure. Focused runs still reproduce failure
before readiness; captured stdout/stderr are empty. Retain
artifacts/hub-virtual-rotator-private-publisher-diagnostics.log and the focused TRX.
A standalone private --hub-init probe succeeded in 5.87 seconds and created its
file. That is timing evidence, not proof of the other helper failures' causes.

Frontend checkpoint 63e7ae4 CI 37518077578/37518073024 now passes all eight jobs,
including Windows packaging and registered COM acceptance. This validates the
outer job budget and pushed native frontend scope, not the newer local virtual
increment. Keep the virtual checkpoint local while resolving managed failures,
then update the same draft PR. Review
also identified missing Focuser/Rotator ProgID generation in the Rust ASCOM identity
helper; address and test it before enabling rotator COM imports. COM, dedicated
simulation, shared creation and every original broader acceptance gate remain open.

Follow-up private CLI probes cover worktree and Unicode temporary paths, direct
and helper-launched hosts, and two simultaneous IPC clients with a Switch lease.
Each publisher reports readiness about 47 ms after spawn and lists exactly three
simulated scalar devices with zero cameras. Every newly created private host and
publisher is retired by the probe; no vendor source or installed profile is used.
Evidence: artifacts/hub-publisher-startup-*-probe.log. A subsequent focused native
publisher test passes in 820 ms. Full rebuilt-host managed confirmation now passes
all 196 NINA cases and real net48 x86/x64 clients, using
artifacts/hub-virtual-rotator-final-confirmed-{nina,net48}.log. No production or
fixture deadline was increased. These are new passing observations, not proof of
the earlier startup/initialization failures' causes; retain those logs and the
broader reliability/acceptance gate. The virtual implementation checkpoint can
now proceed to its own CI while the remaining original plan stays in scope.

### Rotator COM import review

Extended the existing Windows import worker and Rust backend whitelist instead of
adding another executable or controller. Both negotiate V2/V3 legacy Connected and
V4 asynchronous ownership consistently. All driver access remains on one pumped
STA, with the existing process/job isolation, connection-change uncertainty,
sanitized HRESULT handling and bounded transport. Externally managed ownership
never changes connection or invokes Dispose. No installed vendor class is used.

The seven property readers share the existing rotator Boolean/Single-range
validator. Six exact mutations reject unknown fields, wrong casing/types and
out-of-range input before driver dispatch. Review caught negative absolute angles
underflowing to zero during Single conversion; validation now checks the original
Double and converted Single, including rounding upward to 360. Signed relative
distance and source-owned logical/mechanical/target coordinates are preserved.
Optional missing members stay Unsupported rather than guessed values. Dispatched
vendor ArgumentException/COM failures remain uncertain; clearing the fixture fault
cannot replay a move, Reverse or implicit Halt.

Corrected the Rust ASCOM identity helper's missing Focuser/Rotator ProgIDs. Explicit
cross-language class/prefix vectors and canonical case-insensitive self-cycle tests
cover renaming and separate hub instances. Actual worker alias-denial rejects a
registered class before activation, retaining the existing whole-output CLSID
deny list. Shared creation remains gated until its own implementation is ready.

The private fixture now selects its device class from --device-type. One VARIANT
Move method preserves existing Int32 focuser testing and Single rotator inputs,
without AutoDual overload aliases. Source coordinates stay separate, Sync does not
move mechanical position, and trace records prove signed dispatch. Twenty-four
worker tests pass across x86/x64. Fourteen actual registered Rust parent tests pass,
including existing scalar/focuser ownership and new rotator shared leases,
all commands, optional/malformed data, modern reversal admission and uncertainty.
Evidence: artifacts/hub-rotator-com-local-confirmed.log. The first parent run's
13-pass/1-fail log is retained in artifacts/hub-rotator-com-local.log: its assertion
compared JSON 20 with 20.0. The assertion now compares strict numeric values without
requiring a lexical representation; no production validation/deadline was relaxed.
Broad regression/compiler checks and exact-head CI are recorded after completion.
Vendor, conformance and all remaining original milestone gates stay open.

Cross-path review also found the Rust property decoder accepted an angle that
rounds to 360 and a positive StepSize that underflows to zero as Single, while the
shared C# validator rejected both. Rust now checks converted representability as
well as original ranges; absolute commands reject rounding to 360 before dispatch.
Existing real actor regressions cover all three coordinate properties, all three
absolute/reference commands, tiny positive StepSize, oversized values and the
nearest valid Single below 360. Signed relative semantics are unchanged. Final
checks use the refined code, not the preceding pre-refinement passing binaries.

Refined-code acceptance passes all twenty-four worker and fourteen actual registered
parent cases in artifacts/hub-rotator-com-single-confirmed.log. Full Rust hub/Alpaca
regressions (including twenty-two rotator cases), warnings-denied Clippy, Rust 1.89
all-target checks and generated-contract freshness pass in
artifacts/hub-rotator-com-final-{rust,clippy,msrv,contract}.log. Registered proof comes
from the script-owned COM run; ordinary Cargo COM cases without its environment
return without activation. The production host is rebuilt before managed checks.
Rebuilt-host managed confirmation passes all 196 warnings-denied NINA tests and
real net48 x86/x64 clients in artifacts/hub-rotator-com-final-{nina,net48}.log.
No source, production or fixture deadline changed. These new passes do not prove
the causes of the previously retained intermittent failures. At this observation,
virtual checkpoint 87ca1c1 has seven successful jobs in both PR/push CI
37522869618/37522861252; Windows installer acceptance is still running.

Both virtual-checkpoint CI runs subsequently completed successfully with all eight
jobs, including Windows packaging, private COM imports and installer acceptance.
This is exact-head evidence for 87ca1c1. The locally verified COM checkpoint
89fae59 now proceeds into the same draft PR; dedicated simulation remains separate.

### Dedicated rotator simulation review (2026-10-06)

Added the simulator to the existing backend, actor, typed controller and output
fixtures. A shared motion-target enum retains focuser behavior while completing
rotator logical/mechanical angles after a monotonic duration. Sync changes only
the source-owned logical reference; Disconnect cannot Halt. No raw motor encoder
or persistence across new test runtimes is implied. Optional properties, modern
reversal admission, stalled/stopped-short motion and malformed readings pass
through ordinary controller validation. A dispatched uncertain mutation happens
once and fences every subsequent command even after its injected fault is cleared.

Simulation patches validate a cloned candidate before committing. Coordinate,
motion and Reverse changes replace pending test motion; other fields do not.
Source-class checks reject irrelevant controls and faults. Nested virtual fixtures
verify source coordinates, original sample age and simulation labels. The first
new Rust run passed twenty cases and failed an immediate zero-lease assertion:
simulation update briefly owns a lease whose release is queued after its reply.
The fixture now uses the existing bounded eventual assertion and then verifies
disconnected transport. The retained failure is in
artifacts/hub-rotator-sim-local.log; no deadline or production behavior changed.

Both setup frontends consume twelve host-described controls. Shared numeric
validation now checks exclusive upper bounds and Single representability;
nested state groups derive from control paths instead of another device-specific
branch. This also retains strict Switch/weather membership checks. The larger
update exposed Clippy's large-enum warning at the IPC boundary. Boxing that payload
preserves serialized JSON and schema while reducing the command enum's size.
The original diagnostic remains in artifacts/hub-rotator-sim-final-clippy.log.

Twenty-one simulation cases and the dedicated HTTP integration pass. The HTTP
fixture uses actual private IPC and two independently owned published outputs.
All 199 warnings-denied NINA tests and real net48 x86/x64 clients pass before the
IPC representation refinement. Actual WPF acceptance selects and applies only
Logical angle 42.5; its screenshot is explicitly labelled simulation. Runtime
contract and final rebuilt-host confirmation for the boxed payload pass too.
Evidence: artifacts/hub-rotator-sim-boxed-{rust,clippy,host-build,nina,net48}.log and
artifacts/hub-rotator-sim-final-{msrv,contract,node,schema}.log. The final managed
run again passes 199/199 NINA tests and both net48 architectures against the fresh
production host. No test or transport deadlines changed. This increment needs its
own CI; the preceding COM checkpoint has seven successful jobs with Windows
installer acceptance still running in both PR/push runs. Browser interaction,
shared rotator creation, conformance,
equipment acceptance and every original remaining gate stay open.

The preceding COM checkpoint 2ed2578 subsequently passed all eight jobs in both
PR/push CI 37526619358/37526612935, including Windows registered import, packaging
and installer acceptance. Publish the locally verified simulator checkpoint to
the same draft PR; shared creation is a separate increment.

### Shared rotator creation review (2026-10-06)

Enabled the completed rotator class through generated configuration metadata and
the host's `rotatorOutputs` capability. Installed COM rotator choices reuse existing
worker/bitness gates. No frontend implements another device form, controller or
executable. Camera, wheel and panel proxy classes remain unavailable. Configuration
review still checks class relationships and immutable identities before Apply;
editing choices cannot authorize unsupported source construction or bypass leases.

Parameterized the existing native focuser-creation fixture to cover rotators too.
Both cases create two outputs sharing one source, reject unsupported or mismatched
classes, preserve IDs on reload and verify independent NINA leases. The rotator
case checks shared Sync and signed relative movement. Real net48 x86/x64 clients
now create and use their own rotator outputs through the same editor. Actual WPF
button/combobox events cover source/output creation, review/apply/reload and cached
health with zero leases. The first WPF run saved correctly but its final check used
the fixture's original source list; the check now uses authoritative saved UUIDs.
Retain artifacts/hub-rotator-setup-wpf.log.

The initial WPF render captured an empty Review tab. Pixel diversity alone was
insufficient evidence that settings were visible. Acceptance now explicitly selects
Configuration, checks the saved reference value and visibility, then captures it
after the normal dispatcher/layout pass. The first visibility assertion ran before
that pass; retain artifacts/hub-rotator-setup-wpf-visible.log. Final visible capture
passes in artifacts/hub-rotator-setup-wpf-visible-confirmed.log and is inspected.
No production behavior or deadline changed to repair these fixture assertions.

Actual in-app browser acceptance starts from a private empty hub with no camera
profiles. It rejects a focuser output referencing a rotator, then saves/reloads two
rotator outputs numbered 7/8 sharing one source. Sparse Logical angle 42.5 preserves
mechanical angle, target and StepSize. Angles rounding to 360 and step underflow
are rejected before dispatch; Read current state reconciles without Replay.
Visible cached health confirms unchanged revision, zero leases and disconnected
transport. Evidence: artifacts/hub-rotator-setup-browser-{saved,verification}.json
and the actual screenshot. Chrome is unavailable in this session; this is in-app
browser evidence, not a Chrome claim. Both private owned processes and the tab
are retired after acceptance; no hardware or installed vendor driver is opened.

The first Rust regression still asserted that rotator creation was hidden; update
that earlier gate test to require the completed capability and retain rejection of
unimplemented broader proxies. Its original failure is retained in
artifacts/hub-rotator-setup-rust.log. Full refined-code Rust hub/Alpaca suites,
Clippy, Rust 1.89, generated contracts, Node event checks and four schema tests pass.
All 201 warnings-denied NINA tests pass after the visible WPF refinement, and real
net48 x86/x64 creation/output fixtures pass. Logs use
artifacts/hub-rotator-setup-{rust-confirmed,clippy,msrv,contract,schema,node-final,nina-final,net48}.log.
Simulator checkpoint d4050c1 CI has six successes with Intel macOS and Windows
still live in PR/push 37530345334/37530337700. Retain this creation checkpoint
locally until that CI finishes, then push it to the same draft PR for its own CI.
Broader conformance/vendor/interactive acceptance and all original remaining
typed devices, camera ownership/transport, coordination and final gates stay open.

### Accessory lost-reply CI evidence (2026-10-06)

Simulator d4050c1 PR run 37530345334 completes with seven successful jobs but
Windows fails NativeFocuserLostMoveReplyRetainsSharedUncertaintyAndNoReplay:
expected `uncertain`, received `transient`. Retain the completed job log at
artifacts/hub-rotator-sim-windows-failure.log. The test's connection diagnostics
cannot explain this later failure. Review confirms FocuserSession performs live
read-only limit/motion checks before dispatch; their transport failure can return
Transient, whereas a dispatched timeout is retained as Uncertain by the actor.
Existing paused-time coverage verifies that distinction. Neither a preflight
failure nor a fixture scheduling cause is proved by the CI assertion.

Added a shared failure-only helper to the focuser and rotator lost-reply tests.
It captures write/Halt counts and private request trace before diagnostic IPC,
then reports source health without replacing the original semantic failure if
that diagnostic also fails. Successful assertions perform no additional I/O.
There are no retries, deadline changes, production changes or relaxed expectations.
The two focused cases and full warnings-denied NINA suite pass (201/201), recorded
in artifacts/hub-accessory-move-evidence-{focused,nina}.log. These passes do not
resolve the CI cause; push run 37530337700 remains live. Publish the evidence with
the locally verified shared-creation increment for subsequent CI, retaining the
failure and all original remaining gates.

The preceding d4050c1 push run 37530337700 subsequently completed with all eight
jobs successful, including Windows packaging, registration, installer and camera
kit checks. This is retained alongside the failed PR run, not as proof of its
cause. The creation and diagnostic increments can now proceed to their own CI.

### Typed filter-wheel controller review (2026-10-06)

Implemented the initial controller in the existing hub crate using the common
TypedSourceSession, source actor and leases. Required metadata follows
[IFilterWheelV3](https://ascom-standards.org/newdocs/filterwheel.html): ordered names,
signed Int32 offsets with a zero reference, matching slot counts and Position -1
during motion. Connection checks metadata/position within its configured deadline;
moving is a valid initial state. Each move rechecks current metadata and stationary
position while holding unique command control. Writes acknowledge start only.
No automatic focuser offset adjustment, wheel calibration or invented Halt occurs.
Generation loss and canceled/lost writes preserve the common fences.

Resource limits are explicit Regain bounds: 1024 slots, one MiB of UTF-8 name data.
Strict decoding rejects wrong types, mismatched arrays, overflow, missing reference
and out-of-range readings. Duplicate, blank and Unicode source names are preserved.
The host does not synthesize missing imported-driver metadata. Native sources and
all wheel publication remain gated; this increment does not enable a wheel proxy.

The first test compile referred to `connection` instead of `connection_info`;
retain artifacts/hub-wheel-controller-focused.log. The first executed suite passed
nine of ten cases but a fresh connection timed out after a failed preflight.
Added source-state evidence reproduces the cause: the common cache rejected wheel
arrays and scheduled its ordinary 30-second permanent-error backoff. Evidence:
artifacts/hub-wheel-controller-{focused-confirmed,preflight-evidence}.log.
The shared cache now accepts flat scalar metadata arrays, bounded to 1024 elements
per array and 4096 across retained/new entries. Array/scalar strings share the
existing one-MiB aggregate bound. Review verifies partial replacement/removal
excludes old entries; nested arrays, objects and null remain rejected. Camera
images are deliberately outside this cache and require the original buffer/transport
work. No polling, connection or operation deadline changed to fix the failure.

Eleven wheel cases cover independent leases, live limits, invalid/oversized data,
pending/canceled connection, preflight cancellation versus dispatched uncertainty,
concurrent control, generation loss, and actual loopback Alpaca V2/V3 connection
negotiation, arrays and lost-reply no-replay behavior. Two additional source cases
verify array bounds, partial aggregate limits and unchanged invalid-cache behavior.
The cancellation fixture initially used unwrap_err with a non-Debug session;
the corrected join-result match preserves the cancellation assertion. Retain
artifacts/hub-wheel-controller-cancellation.log.

Final full Rust hub/Alpaca suites pass, including all eleven wheel cases and
twenty-six source actor cases. Clippy with warnings denied, Rust 1.89 all targets,
generated contracts, Node and four schema tests pass. A freshly rebuilt production
host passes all 201 warnings-denied NINA tests and real net48 clients in both
Windows architectures. Evidence: artifacts/hub-wheel-controller-{rust,clippy,msrv,
contract,node,schema,host,nina,net48}.log, plus focused cancellation confirmation.
Review found no remaining controller/cache issue in this increment. New exact-head
CI remains required. Next: native metadata/worker adapters, runtime/IPC and all
wheel outputs, imports/virtual/simulation/setup, then panels, cameras/coordination
and every original remaining acceptance gate.

### Native filter-wheel metadata review (2026-10-06)

Direct EFW sources now supply Names and FocusOffsets through the existing native
worker adapter. Optional shared configuration preserves Unicode/blank names and
signed offsets in slot order. Absent metadata uses numbered filter names and zero
offsets for the actual slot count. Explicit arrays must pass common validation
and match the hardware count; they cannot silently fall back to those defaults.
Metadata edits preserve physical source identity, saved IDs and unrelated settings.
Imports retain their driver-owned metadata. No new crate, worker or frontend form
is introduced, and wheel proxy publication remains gated.

Review found a bypass in the low-level native Position write path: the typed
controller checked saved metadata, but direct writes needed the same slot-count
check. Both now reject mismatched explicit metadata before the worker Move request.
Constructor validation rejects metadata for other device classes or malformed
offsets before launching a worker. Calibration and worker recreation preserve the
saved arrays; calibration remains explicit and does not adjust a focuser.

Independent schema tests reproduced acceptance of Int32 overflow because the
generated `format: int32` is only an annotation. The common item schema now emits
explicit minimum/maximum and default zero. Both editors and independent validators
consume those same bounds. Retain artifacts/hub-wheel-native-schema.log and the
successful schema-bounds/Node-bounds confirmations. Semantic class/count/text
validation still belongs to engine review.

Retain the first native focused failure at artifacts/hub-wheel-native-focused.log:
the test incorrectly expected timed ordinary motion from the production EFW USB
simulator, whose ordinary moves apply immediately. The corrected exact stationary
position assertion matches that simulator; the separate two-second calibration
case still requires moving -1, provisional slots and unchanged arrays until done.
No timing or production behavior was changed. All eighteen native tests and
sixteen config tests pass, including actual production workers in explicit
simulation, independent leases, metadata reload, calibration and rejected mismatch.

The first editor test had an empty source label; captured engine errors prove
that rejection. The corrected fixture supplies a label and explicitly checks
filterWheel field paths for class/reference failures. Pre-Apply source inspection
is locally rejected until the source is saved; its exact exception expectation
now follows the existing client contract. Retain the focused/review-evidence/
focused-confirmed logs. The final focused test and all 202 warnings-denied NINA
tests pass, including defaults, Int32 boundaries, review/apply/reload/removal,
stable identities and zero equipment connections. Actual net48 x86/x64 regressions
pass with zero build warnings. Full Rust hub/Alpaca suites, strict Clippy, Rust
1.89 all-target checks, generated-contract freshness, Node and all five independent
schema tests pass. Final evidence: artifacts/hub-wheel-native-{rust-final,clippy,
msrv,contract-final,node-bounds,schema-bounds,nina-final,net48}.log.

Exact bf15ced PR CI 37533746765 has seven successful jobs but fails Windows net48
x86 at a rotator Sync with `uncertain`, after all 201 NINA tests pass. The short
original message omits which of the two Sync paths failed. Retain
artifacts/hub-wheel-controller-ci-windows-failure.log. Failure-only checkpoints now
capture loopback versus simulated stage, private request/reply timings, dispatch
counts, source health and full exception stack without masking diagnostic IPC
failure or adding successful-path reads. No retry, assertion, deadline or production
change is made. Local x86/x64 passes do not prove its cause; push CI 37533737225
must finish, and later exact-head confirmation remains required.

Push 37533737225 subsequently completes all eight jobs successfully, including
Windows packaging, installer acceptance and camera-kit checks. Retain this beside
the failed PR run; it does not establish a cause. Native metadata commits 0c3e22b
and 630b302 are pushed into draft PR #21; exact-head PR/push CI
37536023962/37536015902 is now live.

Next: shared wheel poll plans, runtime/IPC, all three outputs, COM/virtual/simulation
and creation; then panels, camera ownership/transport, coordination and every
original acceptance/final gate. This checkpoint does not close those requirements.

### Shared wheel polling review (2026-10-06, local increment)

The common PropertyPoll replaces the scalar-only name and accepts bounded
Strings/Int32s alongside existing scalar types. Wheel property descriptors build
the poll requests; factory construction deduplicates their three keys across
multiple proxy outputs and scalar Position mappings. Typed keys still count
toward the union limit. Factory construction does not connect equipment or mutate
saved config. This does not enable a wheel runtime/output or another COM class.

Review identified a collection-bound issue in the proposed array path: complete
Alpaca polling counted only scalar text, so many individually bounded arrays could
accumulate before the source cache rejected them. SampleBudget now holds the
existing flat-array/text admission in one place, reused for prospective cache
contents, individual typed values and complete collected poll results. Array
length, aggregate items and aggregate UTF-8 bounds retain saturating arithmetic;
nested arrays/objects/null remain invalid. Partial cache validation still includes
retained values and excludes replacements/error removals. The complete transport
collector rejects before publishing an over-budget set. Safety/number semantics,
per-request polling, retry accounting, ownership and deadlines are unchanged.

The incremental and complete Alpaca paths now call the same typed value check.
Malformed offset arrays produce per-key errors while names/position continue;
the next valid sample replaces that error. Signed Int32 endpoints, Unicode/blank
names and moving -1 are preserved. Slot-count/reference semantics stay in the
wheel controller; generic transport array types do not invent device behavior.

Focused verification passes 27 actual Alpaca transport cases, seven factory
cases, eleven wheel-controller cases and twenty-six source-actor cases. Three new
transport cases cover per-key recovery, element/length rejection and collected
text/item overflow. A factory case checks two wheel outputs plus scalar Position
share exactly three typed keys; the existing union-limit case now covers wheels.
Strict Clippy passes. Review strengthened the collection test to leave one later
response queued, proving admission stops at the first overflow rather than only
rejecting after reading the whole pass; focused confirmation passes. Full Rust
hub/Alpaca suites, Rust 1.89 all targets, contract freshness, freshly rebuilt-host
NINA 202/202 and real net48 x86/x64 regressions pass. Evidence:
artifacts/hub-wheel-polling-{focused,factory-confirmed,budget-confirmed,clippy,rust,
msrv,contract,host,nina,net48}.log. This verified increment remains local while
native metadata head 630b302 runs its own CI. The original runtime/IPC, publication,
import/simulation/setup and later milestone gates remain required.

### Wheel runtime/IPC review (2026-10-06, local increment)

The common runtime now builds typed FilterWheel sessions, deduplicates polling
through the existing factory, and routes private get/move operations. No raw
vendor command, calibration or fabricated Halt is added. Setup creation and all
three publications stay gated until their adapters and acceptance are complete.
The controller continues to own command preflight and generation/uncertainty
semantics; runtime wiring does not duplicate them.

The typed sample envelope is shared with focusers and rotators, preserving wire
fields, per-key sequences and clock/epoch semantics. Their health and value
decoders still run first. Wheel cached reads validate matching metadata and live
slot bounds before availability, and retain the oldest dependency age. Diagnostics
and standard Position-only DeviceState do no I/O. Array descriptors and generated
schema drive both frontend diagnostic readers, including zero-reference and
signed Int32 bounds. Summaries preserve array boundaries and empty/Unicode names.

Four private runtime/IPC cases cover inert cached paging, mismatched metadata,
dependency age, cancelled connection admission, preflight generation loss,
sparse output identities, sibling/EOF leases, unknown Move fencing and escaped
metadata response overflow without stream loss. All fifteen wheel, eighteen
focuser and twenty-two rotator cases pass. A new runtime test runs the actual
production EFW worker explicitly in simulation; it verifies saved arrays,
cached polling, shared leases, last-client cleanup and metadata after reconnection.
Browser reader and five independent schema checks pass. Full Rust hub/Alpaca
regressions, strict Clippy, Rust 1.89, generated-contract freshness, freshly
rebuilt-host NINA 203/203 and actual net48 x86/x64 clients pass. Managed builds
have zero warnings. The new managed diagnostic case uses synthetic wheel replies
and never applies/connects a native wheel source. Both frontend readers reject
malformed/oversized arrays, missing zero, Int32 overflow, invalid Position and
wrong observation identities while preserving empty/Unicode names and offsets.

Retain artifacts/hub-wheel-runtime-focused.log: the first new private fixture
used a nonexistent SourceHandle::id; source identity is read from its snapshot.
Retain artifacts/hub-wheel-runtime-native.log: the first new native fixture
looked for the simulation flag on SourceHealth instead of SourceSnapshot. Both
are corrected test API references; focused confirmation passes. No production
deadlines, assertions or retry policies were weakened. Original publication,
import/simulation/setup, conformance and later milestone gates remain open.

Review strengthened the cache/IPC case with Position-only sample failure and
recovery: metadata remains available, standard DeviceState omits Position without
I/O, and valid polling restores availability. Overflow now checks outputStatus
as well as a direct Names read, then verifies Position and the session survive.
Retain artifacts/hub-wheel-runtime-cache-confirmed.log: the first added
DeviceState assertion attempted a crate-private method from an integration test.
The corrected fixture exercises public framed IPC instead of widening production
visibility. All fifteen wheel cases pass in cache-final.log. Other final evidence:
artifacts/hub-wheel-runtime-{focused-confirmed,native-confirmed,rust,clippy,msrv,
contract-final,node,schema,managed-build,host,nina,net48}.log.

Exact native-metadata head 630b302 PR/push CI 37536023962/37536015902 both complete
all eight jobs successfully. The earlier two Windows reliability failure causes
remain unproved; this success is not a claim to have fixed them. The reviewed
polling and runtime increments can now proceed to their own CI in the same draft
PR. Next: wheel Alpaca/native NINA/native ASCOM publication, then imports, virtual
and dedicated simulation inputs/shared creation, panels, cameras/coordination and
every original acceptance/final gate.

### Wheel Alpaca publication review (2026-10-06, local increment)

FilterWheel joins the existing Publisher capability gates, class naming and typed
get/put translation. The three source properties reuse their controller
descriptors; Position uses strict signed Int32 parsing and live controller
preflight. Modern connection/DeviceState negotiation stays shared with other
outputs. No vendor passthrough, calibration or fabricated Halt is exposed.

Review covers route identity and coexistence: dynamic wheel numbers resolve from
the host catalog, local wheel 0 remains available when separately configured,
and a selected local 0 rejects a hub 0 collision before opening equipment. The
hub can use 0 when the local profile is unselected. Setup-page routing for all
three typed classes now shares one helper, preserving native fallback/404/503
behavior and preventing a fixed local setup route from shadowing a hub wheel.
This does not enable unfinished shared wheel creation.

The existing private accessory upstream fixture now supports wheel V2/V3, using
the class-specific modern connection boundary while retaining focuser/rotator
behavior. Five new actual HTTP/private-endpoint cases cover two sources, sparse
output identities, ordered Unicode/blank names and signed offsets, independent
leases, modern and legacy connection methods, exact Position parameter routing,
moving state, malformed metadata/values, sanitized upstream errors, no writes
on failed preflight, local coexistence/collision and unknown Position replies
without replay or extra commands. One case runs the production EFW worker in
explicit simulation and preserves metadata/shared position through HTTP.

The first four focused wheel cases pass. After factoring setup routing and
strengthening malformed-metadata coverage, all 31 HTTP router cases pass,
including five wheel cases and existing focuser/rotator/scalar regressions.
Final full Rust hub/Alpaca regressions, strict Clippy, Rust 1.89, generated
contract freshness, Node/five independent schema checks, freshly rebuilt-host
NINA 203/203 and real net48 x86/x64 clients pass. Both managed fixture builds
have zero warnings. Evidence: artifacts/hub-wheel-http-{focused,router,rust,
clippy,msrv,contract,node,schema,host,nina,net48}.log. No new failures, deadline
changes, weaker assertions or retry changes were introduced. The preceding
runtime PR CI 37539206029 has six successes with Intel macOS and Windows live;
push 37539201271 remains live. Keep this local increment until they finish, then
publish into the same draft PR. The original native publication, imports,
virtual/dedicated simulation, shared creation, conformance and later milestones
remain required.

### Native wheel publication review (2026-10-06, local increment)

NINA and ASCOM share a bounded wheel request/value validator in the existing
native hub library. Strict JSON types, 1..1024 slots, aggregate UTF-8 bounds,
signed Int32 offsets with a zero reference, and Position -1..1023 match Rust.
Rust remains the authority for live pairing, slot bounds, command control and
generation fences. Existing sessions, saved choices, metadata-only factories
and registration implement the new class without a parallel server or worker.
Rust/C# stable identities use wheel prefix Rgn.HL.; Weather retains Rgn.HW.
Independent UUID vectors and self-proxy checks cover wheel exports.

NINA preserves existing FilterInfo objects, adds source defaults for missing
slots and trims absent slots. Metadata is validated before publication, and a
changed active profile rejects publication into either collection. The short
Position setter returns on acceptance; actual reads retain -1 while moving and
never substitute the requested target. ASCOM exports V3/V2 with actual COM QI
coverage and Short Position in cached standard DeviceState. Neither frontend
applies offsets, introduces calibration/Halt, nor silently retries motion.

Five private NINA wheel cases and the wheel registration theory cover saved
choices, wire/resource boundaries, profile preservation/change, malformed live
state, nonblocking sibling motion and unknown Position replies. Real net48
x86/x64 clients exercise both interfaces, signed metadata boundaries, actual
Position, standard state types, independent disconnects and no-replay fencing.
Seven-output manual export tests independently derive identities and exercise
both server and client bitnesses, array marshaling and cached Short state. The
PowerShell test constructs Unicode expectations from code points so Windows
PowerShell's script encoding cannot corrupt them.

The first focused run passes six of seven cases; one fails before its assertions
because NINA's collection captures xUnit's headless synchronization context. The
test now uses the same context-free Task.Run pattern as the runtime wheel cases.
All seven focused cases pass; the added profile-change case passes in both full
209-test runs. Final Rust hub/Alpaca suites, strict Clippy, Rust 1.89 all targets,
generated-contract freshness, Node/five schema checks, freshly rebuilt-host NINA
209/209 and real net48 x86/x64 clients pass. Both-architecture ASCOM staging has
zero warnings. Logs: artifacts/hub-wheel-native-output-{focused,
focused-confirmed,rust,clippy,msrv,contract,node,schema,host,nina,nina-final,
net48,net48-final,stage,exports}.log. Cold HKCU SCM activation fails at the first
existing Switch class before wheel activation, matching the retained local
limitation; artifacts/hub-wheel-native-output-scm.log preserves the failure.
Disposable registered/cold wheel CI and interactive/conformance acceptance remain
open. No vendor driver or physical equipment was activated.

Runtime push CI 37539201271 completes with two failures. Windows initial focuser
Connect returns uncertain before the lost-Move assertion: PUT connected closes
without a reply. Its cause is unproved; existing private failure-only traces now
include caught transport exceptions. Intel macOS asserts final upstream cleanup
immediately after local Connecting=false. SourceLease::drop explicitly schedules
cleanup asynchronously, so the test now observes upstream disconnection within
the existing three-second budget, retaining the assertion and checking the
independent source stays connected while waiting. Full local Rust suites pass;
portable CI must confirm. Evidence: artifacts/hub-wheel-runtime-ci-failure.log.
PR 37539206029 has seven passing jobs with Windows installer acceptance live.
Keep this reviewed increment local until it finishes, then publish into the same
draft PR. Wheel COM/virtual/simulation/shared creation and all original remaining
milestones and final acceptance gates remain required.

### Windows COM wheel import review (2026-10-06, local increment)

The existing isolated worker now whitelists FilterWheel Names, FocusOffsets,
Position and the Short Position setter. V2/V3 retain the common legacy/modern
connection state machine, borrowed ownership, strict parameters and sanitized
errors. Before serializing vendor SAFEARRAYs, the worker bounds rank/count,
checks every element type and counts strict UTF-8 bytes, rejecting malformed
surrogates. It then uses the shared native validator, including a zero reference
offset. Rust admits arrays only on wheel Names/FocusOffsets reads and applies
the existing bounded controller decoder; unexpected arrays/objects still retire
the corrupt transport. Shared polling, typed controller slot/metadata validation,
leases, generations and no-replay uncertainty are reused without new processes.
Generated wheel COM choices remain gated until shared wheel creation is complete.

Private driver extensions preserve existing scalar/focuser/rotator fixtures and
require Short wheel setters. Three new worker cases cover V2/V3 in both bitnesses,
Unicode/blank names, signed offsets, moving -1, type/rank/count/text bounds,
unsupported commands, strict parameters and ambiguous setter errors. Borrowed
ownership/alias-denial coverage is shared with rotators. Two registered Rust
parent cases prove one worker for sibling outputs, actual typed array polling,
signed boundaries, failed admission, independent disconnects and retained source
uncertainty without replay/Halt. No installed vendor driver was activated.

The first 27-case worker run fails the wheel invalid-slot assertion in both
architectures: the fixture used 0x80040405 instead of ASCOM InvalidValue 0x80040401.
The worker correctly returned uncertain for that unrecognized setter failure.
After correcting the fixture constant, all 27 worker cases pass. The first actual
parent run then passes 14/16, with both wheel cases receiving Connecting. Review
finds the old unconditional array rejection retires the worker at metadata reads;
the narrowed, bounded array admission above fixes the production gap. Final
private worker/registered parent confirmation passes 27/27 and 16/16, including
existing timeout, cancellation, safety, corruption, alias and ownership cases.
Evidence: artifacts/hub-wheel-com-{focused,focused-confirmed,parent-confirmed}.log.
Full Rust hub/Alpaca, strict Clippy, Rust 1.89 all targets, generated-contract
freshness, Node/five independent schema checks, freshly rebuilt-host NINA 209/209
and real net48 x86/x64 clients pass. Evidence: artifacts/hub-wheel-com-{rust,
clippy,msrv,contract,node,schema,host,nina,net48}.log. The worker/fixture and both
managed architecture builds have zero warnings. No production/test deadlines,
retry rules or assertions were weakened.

The preceding wheel publication head 0669339 is pushed to draft PR #21. Its
PR/push CI 37541898392/37541893308 remains live. Runtime head 08913c8 PR CI
37539206029 passes all eight jobs; its separate push failures remain retained.
Keep this import increment local until reviewed local checks and preceding CI
finish. Virtual/dedicated wheel simulation/shared creation, panels, cameras,
coordination and every original acceptance/final gate remain required.

### Virtual wheel input review (2026-10-06, local increment)

Virtual wheels reuse the supervised typed-accessory connection path and existing
controllers instead of adding another worker or connection owner. Review covers
bounded handshake readiness, cancellation, generation retirement, shared command
leases, strict Position parameters and retained uncertainty. Cached forwarding
uses the wheel controller's metadata pairing and oldest dependency age, preserving
arrays and per-key errors without device I/O. The small Int32 parameter helper
also preserves the existing focuser validation behavior and error messages.

Six two-layer loopback cases exercise V2/V3 ownership, a deliberately slow
handshake under shorter outer request budgets, cancellation, lost inner generation,
unknown Position acknowledgment without replay, actual moving -1, metadata
dependencies, original ages and cache recovery. The production EFW worker test is
shared between direct and two-layer virtual cases in explicit simulation. It
verifies saved metadata, movement, sparse output numbers, simulation diagnostics,
independent lease release and reconnection without changing saved metadata.

The first focused run passes 20/21 wheel cases. Its cache case expected
Unavailable for malformed wire Position, although the Alpaca sampler reports
Permanent and the virtual path correctly retains that error. The corrected
assertion requires the original classification and intact metadata at every
source layer, while the live getter retains its existing Unavailable result.
All 21 wheel and 20 native cases then pass, as do Node and five schema checks.
Evidence: artifacts/hub-wheel-virtual-{focused,cache-failure,
focused-confirmed,node,schema}.log. Full Rust hub/Alpaca regressions, strict
Clippy, Rust 1.89 all targets, generated-contract freshness, freshly rebuilt-host
NINA 209/209 and real net48 x86/x64 clients pass. Managed builds have zero
warnings. Evidence: artifacts/hub-wheel-virtual-{rust,clippy,msrv,contract,host,
nina,net48}.log. No deadline, retry rule or production error was changed. No
physical equipment or installed vendor driver was activated.

Preceding head 0669339 PR/push CI 37541898392/37541893308 has seven successful
jobs with Windows build/installer checks live. Both Windows test.ps1 steps have
completed successfully, including private SCM/production wheel registration
acceptance. Local HKCU SCM restrictions and the earlier unexplained transport
failures remain recorded; later passes do not prove their causes resolved.

Dedicated wheel simulation/shared creation, panels, cameras, coordination and
every original acceptance/final gate remain required. Keep one draft PR #21;
preceding publication CI must finish before these local increments are pushed.

### Dedicated wheel simulation review (2026-10-06, local increment)

The existing simulator now supports FilterWheel using the same actor, typed
controller, timed motion, sampling ages, connection leases and uncertainty fence.
Metadata validation reuses NativeFilterWheelMetadata; sparse updates validate a
copy before assignment. Position/metadata injection replaces pending movement,
while duration/age/fault patches retain it. Accepted moves report -1 until the
monotonic deadline; disconnect cannot Halt or substitute a requested target.
StoppedShort retains the prior slot, InvalidMotion corrupts only Position, and
UncertainWrite applies once while retaining the shared fence after fault clear.
No wheel Halt, calibration or focuser offset application was introduced.

Both setup frontends consume generated bounded JSON-array controls. Validation
retains strict Unicode byte limits, Int32 bounds, a zero offset reference, slot
order and blank names. Status validation also checks array pairing and actual
Position bounds. Review consolidated the typed simulator write/fault return path
for focusers, rotators and wheels. Dedicated controls do not yet enable shared
wheel creation in the generated forms.

The first focused build failed on test-only json! repetition syntax; using Vec
fixes it. All 26 simulator cases then pass. Independent JSON Schema validation
finds that schema_with made optional FocusOffsets required; serde(default) now
retains sparse patch semantics, and all six schema checks pass. The first HTTP
case expected the generic driver code for busy; the existing adapter correctly
maps it to 0x40b. Its corrected assertion and the HTTP case pass without changing
production error mapping.

Review also identifies an applied-but-oversized reply. A new actual framed IPC
case proves a valid update can apply before responseTooLarge, without terminating
the stream. Explicit smaller repair succeeds without truncation, replay or larger
frame budgets. Both setup frontends now revoke review and require reload after
that response. Existing no-replay frontend cases additionally exercise this error
after one applied update. All 27 simulator cases, the HTTP case, six focused
NINA/setup cases, Node and six schema checks pass. Evidence:
artifacts/hub-wheel-simulation-{check,focused,focused-confirmed,contract-generate,
schema,contract-confirmed,schema-confirmed,rpc,http,http-confirmed,host,
node,node-confirmed,nina-focused}.log.

Strict Clippy found that the larger inline simulation update inflated the actor
command enum. Boxing that payload follows the IPC command design and avoids
enlarging every queued command. Review also corrected local browser JSON parse
errors to carry invalidValue; invalid input must not be presented as an unknown
remote mutation. Full Rust hub/Alpaca suites, strict Clippy, Rust 1.89, generated
contract freshness, Node/six schema checks, rebuilt-host NINA 212/212 and real
net48 x86/x64 clients pass. Logs additionally include
artifacts/hub-wheel-simulation-{clippy-confirmed,msrv,rust-confirmed,contract,
host-final,node-final,nina,net48}.log. The actual WPF capture was visually checked;
the workflow applies only selected Names, retains offsets/Position and verifies
zero source leases. This is explicit simulation acceptance, not hardware proof.

Preceding publication head 0669339 passes all eight jobs in both PR/push CI
37541898392/37541893308. Reviewed COM/virtual increments are pushed at a586c76
to the same draft PR #21; new PR/push CI 37544747351/37544741219 is live. Earlier
unexplained Windows failures remain retained. Shared wheel creation, panels,
cameras, coordination and every original acceptance/final gate remain required.

### Shared wheel creation review (2026-10-06, local increment)

The generated schema enables wheel COM/simulation choices and gates wheel proxies
on filterWheelOutputs. The runtime advertises that configuration capability only
now that native, Alpaca, COM, virtual and dedicated simulation paths exist. Camera
and panel gates remain closed. Neither frontend adds a separate wheel editor.
Parameterized WPF creation and actual net48 creation workflows reuse the rotator
path, retaining saved identity, inert review/apply and independent source leases.
The actual IPC test checks configuration capability publication before connecting.
Node form events exercise both rotator and wheel transitions; six independent
schema checks pass. The first full run caught a test request-ID error: inserting
ID 20 before existing lower IDs correctly closed the stream. A separate sequential
IPC capability test retains the original stream-order checks. Its cleanup now
asserts the shutdown result. No production protocol rule or deadline changed.

All 22 wheel cases and full Rust hub/Alpaca suites pass, along with strict Clippy,
Rust 1.89, contract freshness, NINA 213/213 and real net48 x86/x64 clients.
The latter create two wheel outputs, verify saved identities, metadata/motion and
independent leases. Fresh WPF setup and simulation captures are visually checked.
Review corrects stalled-motion help: clearing the fault resumes motion; Position
injection replaces it. The generated description and both frontends agree.

Actual browser acceptance uses a fresh hub with empty equipment profiles and
only explicit simulation. It creates outputs 7/8 sharing one source, blocks Apply
for a mismatched class, saves/reloads stable IDs, rejects local invalid JSON
without requiring reload and applies only Names. The observed status retains
Position 0, seven zero offsets, zero leases and disconnected transport. Private
processes are stopped after verification; no installed driver/equipment is opened.
Evidence: artifacts/hub-wheel-creation-{focused-confirmed,rust-confirmed,clippy,
msrv,host,contract-final,node-final,schema-final,nina,net48,ipc-final}.log;
artifacts/hub-wheel-creation-browser-{saved,verification}.json; actual captures
in docs/images. Preceding a586c76 CI remains live. Panels, cameras, coordination
and all original remaining acceptance/final gates stay required.

### Typed panel controller review (2026-10-06, local increment)

Reviewed the CoverCalibrator V1/V2 interface against installed ASCOM 7.1.2
declarations and the canonical interface, then reused TypedSourceSession instead
of adding another actor or command arbiter. Cover and light presence are separate.
Brightness validates live positive Int32 MaxBrightness; Off requires zero and
On(0) retains a logical Ready/NotReady state. Acknowledged movement and warm-up
never substitute a target or Ready state. Unknown/Error retains valid presence
and explicit command access without inventing completion.

Only negotiated V1 sources derive completion from enum states. Unknown/Error
cannot become false. Modern/unversioned sources require actual Boolean completion
properties; missing or malformed mandatory properties reject readiness. Failed
second handshakes release only their own leases. Source-generation changes retire
old sessions. Concurrent calls use the existing unique operation lease. Cancelled
preflight never dispatches; cancelled or lost-ack dispatch retains the shared
uncertainty fence without replay, automatic Halt, Close or Off.

Native OFP2 status now retains independent motion evidence from the already-read
GOPS reply. Captured GOPS=1/GPOS=232 after STOP means stopped at an unknown endpoint.
GOPS=3 has no captured completion meaning and remains unavailable. The explicit
simulator models the observed stopped intermediate position. Native Open/Close
checks movement before dispatch and returns Busy without claiming an uncertain
mutation; imported drivers keep their own preemption policy. Native illumination
has no reported warm-up stage, so its completion property follows the existing
Ready/Off interpretation without a fabricated timer.

Fifteen controller cases pass, including actual loopback Alpaca V1/V2 connection
negotiation, shared clients, zero-on, warm-up and an applied-once command with a
malformed acknowledgement. Both production-worker panel cases pass with explicit
SIM-OFP2, including typed sibling clients, live maximum, known stopped/unknown
endpoint and no darkening on sibling disconnect. All ten vendor protocol cases
and the movement-timeout unit test pass. Full Rust hub/Alpaca regressions, strict
Clippy, Rust 1.89 all targets, generated-contract freshness, Node contract/event
checks and six independent schema cases pass. The default Python lacked
jsonschema; the existing artifacts/hub-schema-venv passes without installing or
changing dependencies. Fresh-host NINA 213/213 and real net48 x86/x64 regressions
pass. The standalone OFP2 worker/HTTP simulation also passes brightness, full
open/close, mid-travel halt/resume, discovery, persistent identity, independent
clients, reconnect, invalid values and origin checks.

Evidence: artifacts/hub-panel-{vendor,workers,native-focused}.log and
artifacts/hub-panel-controller-{focused,rust,clippy,msrv,contract,node,
schema,schema-confirmed,host,nina,net48,ofp2}.log. Panel runtime/IPC/diagnostics, every publication,
COM/virtual inputs, dedicated controls and shared creation remain open. No panel
setup gate is enabled by this checkpoint. All later plan requirements remain.

Preceding a586c76 PR/push CI 37544747351/37544741219 now passes all eight jobs.
Verified wheel simulation/shared creation is pushed at 5d0ed34; new PR/push
CI 37547703480/37547695748 is running. Draft PR #21's body describes that pushed
head and does not claim local panel changes are published or hardware accepted.

### Panel runtime, IPC and cached diagnostics review (2026-10-06, local increment)

Reviewed existing source/controller/client ownership rather than adding a panel
host. Dynamic outputs share a source and one deduplicated six-property polling
plan, including combined Switch brightness gauges. Typed IPC uses the existing
Get/Put dispatch and version/capability negotiation. Pending admission,
cancellation, generation loss, wrong-class access and uncertain writes keep the
same lifecycle/quiescence fences. Close, Halt and Off are never invented on
disconnect or uncertain completion.

Cached light properties depend only on their required light observations.
Brightness validates MaxBrightness and CalibratorState, including Off=0 and
logical On(0); maximum validates presence. Ages retain the oldest dependency.
Legacy completion uses the source enum and its age even when polling the V2
property returns Unsupported. Unknown completion remains unavailable; modern
errors retain their original class. Cover errors do not erase valid light data.
DeviceState reads one cache snapshot and independently omits unavailable values;
MaxBrightness and a fabricated measurement timestamp are excluded.

Both diagnostic readers reuse their existing typed-accessory paths and generated
types/ranges. New private tests reject Int32 overflow, invalid state enums,
incorrect Boolean values and epoch/type/page faults, while retaining unavailable
completion messages. This is diagnostic support, not a native NINA/ASCOM panel
driver. Selection, creation and all publications remain gated/pending.

The first added fixtures used nonexistent client/registry accessors; compilation
caught these and the tests now use the actual public client/snapshot API. The
first expanded actual Alpaca runtime cases asserted brightness availability as
soon as state properties arrived. Captured snapshots in
artifacts/hub-panel-runtime-alpaca-evidence.log prove completedPasses=0 and absent
MaxBrightness in both V1/V2. Production correctly withheld the dependent value.
The fixture now awaits that observation within its existing three-second budget;
no production poll deadline, assertion or retry rule was relaxed.

Verification: all 19 panel cases, eight factory cases, 22 native cases and full
Rust hub/Alpaca suites pass. Actual loopback Alpaca V1/V2 now exercises the runtime
and configuration-derived poll plan, shared ownership, partial legacy failures,
zero-on, actual motion and applied-once lost acknowledgements. Production OFP2
runtime uses explicit SIM-OFP2 with two outputs and retained light on sibling
disconnect. Strict Clippy, Rust 1.89 all targets, generated-contract freshness,
Node/six independent schema checks, warning-denied managed build, fresh-host NINA
214/214 and real net48 x86/x64 regressions pass.

Logs: artifacts/hub-panel-runtime-{check,focused,focused-confirmed,native,
native-confirmed,contract-generate,contract,node,schema,rust,rust-confirmed,
alpaca-evidence,clippy,clippy-final,msrv,msrv-final,host,managed-build,nina,net48}.log.
Keep this locally verified increment with f622d51 until preceding 5d0ed34 CI ends.
Both PR/push runs have seven successful jobs and Windows build/installer work
remaining at last observation. All original remaining plan gates still apply.

### Panel Alpaca publication review (2026-10-06, local increment)

Reviewed the existing publisher, capability negotiation, typed Get/Put dispatch,
common connection ownership and cached DeviceState. Panel publication adds no
host, acquisition actor or frontend-specific state machine. Saved UUIDs and
noncontiguous class-local numbers survive discovery/routing; independent HTTP
clients and multiple source devices retain distinct leases. V1 completion is
inferred only by the shared controller; mandatory modern errors stay errors.
Brightness remains strict Int32 with live maximum/presence checks and valid On(0).
Cover commands preserve imported-driver preemption rather than impose a generic
Busy rule. Unknown endpoint and known stopped are independent modern properties.

The shared setup-page helper scopes configured panel routes. Standalone OFP2 at
slot zero retains its existing setup/API and identity. A collision rejects
discovery, reads and setup without acquiring a lease or touching equipment.
Applied-once lost acknowledgements fence sibling Close/Halt/Off/On commands,
retire connected state and prevent idempotent Connect from silently adopting a
new generation. Publisher/runtime shutdown sends no extra actuator command.

Five new actual HTTP cases pass for loopback V1/V2 and explicitly simulated
production OFP2. They also cover strict argument errors, absent independent
components, malformed modern completion, live range changes, warm-up, cache
omission, shared upstream ClientID and unique transaction IDs. The first focused
run passed. Review simplified the cache-observation predicate without changing
its budget or the production polling policy. All 37 router cases and full Rust
hub/Alpaca suites pass, as do strict Clippy, Rust 1.89 all targets, generated
contracts, Node/six independent schema checks, fresh-host NINA 214/214 and real
net48 x86/x64 clients. Standalone OFP2 worker/HTTP simulation passes brightness,
full open/close, mid-travel halt/resume, independent clients, discovery and
persistent identity, reconnect, validation and origin checks.

Evidence: artifacts/hub-panel-http-{focused,rust,clippy,msrv,contract,node,schema,
host,nina,net48,ofp2}.log. No physical equipment or installed vendor drivers were
opened. Preceding 5d0ed34 PR CI 37547703480 passes all eight jobs; push CI
37547695748 has seven passed and Windows installer acceptance running at last
observation. Keep panel increments local until that run ends. Native NINA/ASCOM
panel outputs, COM/virtual inputs, dedicated simulation/shared creation and every
original remaining gate remain required; PR #21 stays draft.

Preceding push CI 37547695748 is now terminal success, so both 5d0ed34 runs pass
all eight jobs, including Windows packaging/installer acceptance. The reviewed
panel controller/runtime/HTTP commits may now be pushed; their own CI remains
required. This does not establish hardware or interactive acceptance.

### Native panel publication review (2026-10-06, local increment)

Reviewed the installed NINA 3.2 IFlatDevice interface and ASCOM.DeviceInterfaces
7.1.2 V2/V1 declarations. Both native outputs share one new C# protocol validator,
existing private sessions, output selection/setup styling and registration paths.
No HTTP bridge or second equipment owner is introduced. Provider enumeration,
metadata and registration are inert; saved output class/identity and required
host capabilities are checked before acquiring equipment. Native ASCOM exports
both interfaces, keeps actuator acknowledgements nonblocking and preserves
Brightness as Int32, states as declared enums and completion as Boolean in
DeviceState. Unknown/absent states and independent completion stay source-owned.

NINA maps enum values explicitly, waits for actual cover endpoints and light
readiness/brightness, and bounds completion waits. Its light toggle remembers
only a requested level for the current connection epoch, including logical On(0).
Live readings and source polling remain authoritative. Review removed a possible
lock-order inversion between connection publication and requested-level storage,
and bound the explicit toggle's read/preflight/write to one captured epoch so it
cannot retarget a replacement connection. No lock calls into ReadContext.
Cancelled, stopped-short, invalid or uncertain operations never invent Halt,
Close, Off or a replay. Cover/light absence has independent NINA capabilities;
unknown light state is unavailable rather than falsely Off.

The first NINA compile caught use of an internal HubException constructor outside
its assembly; the unreachable invalid-state branch now reports InvalidDataException.
The first net48 fixture used LINQ Cast on IStateValueCollection, which exposes an
enumerator/indexer rather than IEnumerable; the test now enumerates its actual
indexed interface. Both failures are retained. Seven initial panel cases passed;
an additional actual HTTP-publisher/NINA case proves shared state and independent
leases across processes, including surviving publisher shutdown without Off.
The existing registration theory now covers CoverCalibrator chooser entries and
stable identities in both private registry views.

The first eight-output manual COM export test failed because the new fixture
expected plain Int32 for state enums. Captured type evidence proves actual
ASCOM.DeviceInterface.CalibratorStatus Ready was preserved. The corrected fixture
requires exact declared enum types with Int32 underlying representation and
bounds, plus strict Brightness Int32 and Boolean completion. Production behavior
was not changed to accommodate the fixture. Both server/client bitnesses now pass
real exported-COM metadata, state, commands, cached DeviceState and source sharing.

Final verification: full Rust hub/Alpaca suites, warnings-denied NINA 223/223,
real net48 x86/x64 clients with V1/V2 panel inputs, both-architecture warning-denied
staging, eight-output manual COM exports with both client bitnesses, Node and six
independent schema cases pass. Existing HTTP-head strict Clippy, Rust 1.89 and
generated-contract checks still cover the unchanged Rust sources. Source/fixture
Python syntax and diff checks pass. No physical device or installed vendor driver
was activated. Cold/production panel registration requires the new head's CI;
interactive and hardware acceptance remain separate gates.

Evidence: artifacts/hub-panel-native-{build,build-confirmed,ascom-build,
nina-focused,nina,nina-confirmed,nina-final,cross-frontend,net48,net48-confirmed,
staging,exports,exports-type-evidence,exports-confirmed,rust,node,schema}.log.
The local interface inspection helper is under artifacts/hub-interface-inspect.
Panel controller/runtime/HTTP is pushed at 36a5558; PR/push CI
37550182065/37550174221 is live. Keep this reviewed native increment local until
those runs end. Next: panel COM imports, virtual inputs, dedicated simulation and
shared creation, followed by every original camera/coordination/acceptance gate.

### Windows panel import review (2026-10-06, local increment)

Reviewed the existing STA activation, strict member whitelist, connection policy,
request framing and uncertain-mutation behavior. CoverCalibrator now uses those
paths in both helper architectures, sharing native property validation and the
Rust panel controller rather than introducing another owner or transport.
V1 negotiates Connected; V2 negotiates asynchronous Connect/Disconnect. Borrowed
and external ownership, class alias denial and no vendor Dispose remain common.

Six reads admit strict Int32, state enums 0..5 and Boolean completion. Actual
declared ASCOM enums and COM integer representations are admitted without numeric
string/fraction/overflow coercion. V2 completion remains mandatory; only the
existing V1 controller derives it. Five nonblocking commands validate exact
parameter shapes before invocation. Live presence and maximum checks remain in
the shared controller. Unknown command outcomes fence sibling mutations without
automatic Halt/Close/Off or replay.

Thirty actual private COM worker checks pass, including panel V1/V2 connection
thresholds, all properties/commands, strict invalid readings/arguments, STA pump,
borrowed/external cleanup and class alias denial. The fixture uses the installed
interface package for actual state enums. Its applied-then-failed On records
mutation before throwing, while its applied-lost-reply mode stalls after recording
mutation so the production Rust parent retires the worker.

The first registered parent run passes 18/19 cases. The new lost-reply case passes
its uncertainty fence and no-replay checks but incorrectly requires exactly one
activation through shutdown. The source actor can legitimately reconnect its
poll transport after a loss. The corrected test requires one initial shared
activation and exactly one applied On through shutdown; all sibling commands
remain fenced and no automatic actuator cleanup is permitted. Production recovery,
deadlines and retry rules are unchanged. The original failure log is retained at
artifacts/hub-panel-com-private.log. Confirmation passes all 30 worker and 19
actual registered parent cases, covering V1/V2 in both architectures and retained
uncertainty through sibling calls and shutdown.

Full local Rust hub/Alpaca suites, strict Clippy, Rust 1.89 all targets, generated
contract freshness, Node/six schema checks, rebuilt-host warnings-denied NINA
223/223 and real net48 x86/x64 regressions pass. Python syntax and diff checks
pass. No physical equipment or installed vendor driver was activated. Panel COM
choices stay gated until virtual inputs, simulation and shared creation are
verified. Every original later gate remains required.

Evidence: artifacts/hub-panel-com-{compile,private,private-confirmed,rust,clippy,
msrv,contract,node,schema,host,nina,net48}.log. Previous panel CI remains live;
keep reviewed native/import increments local until that preceding run finishes.

### Virtual panel input review (2026-10-06, local increment)

Reviewed reuse of the existing validated composition graph, supervised bounded
typed connection, generation-fenced sessions, IPC command mappings and source
leases. Panels use that path with six strict properties and five commands. Their
cached-sample accessor delegates to the existing panel cache validator; polling
forwards values, errors and original dependency ages without leaf I/O. Int32
parameter admission is now shared with wheel/focuser forwarding. Virtual panel
metadata publishes V2 while the leaf retains V1/V2 ownership and completion rules.

Six actual two-layer loopback cases cover legacy/modern states, independent
completion, logical On(0), live brightness bounds, cache aging without extra leaf
requests, partial errors/recovery, slow/cancelled connections, lost preflight
generations and applied malformed acknowledgements. A seventh case uses the
production OFP2 worker in explicit simulation through two virtual layers and
checks shared illumination, cover/Halt status, simulation provenance and leases.
All 25 panel and four focused native panel cases pass.

Initial failures were fixture assumptions. A numeric zero maximum is valid to the
scalar sampler but rejected by the typed cache validator; assertions now check the
raw leaf sample and dependent errors in virtual layers. Captured snapshots prove
an inner source can become idle after retiring its last lease, clearing its local
latch under existing disconnect rules. The corrected uncertain-command assertion
requires the outer owner and every still-owned source to remain fenced, plus one
applied actuator command through shutdown. No production recovery or timing rules
were changed. The first evidence compile also caught use of id instead of the
actual SourceSnapshot.source field. Failure logs are retained.

Final confirmation: all 25 panel and 23 native cases and full Rust hub/Alpaca
suites pass, along with strict Clippy, Rust 1.89 all targets, generated contract
freshness, Node/six schema checks, rebuilt-host warnings-denied NINA 223/223 and
real net48 x86/x64 clients. A concurrent host build encountered a Windows lock on
the test-owned executable; the original log is retained. After the actual Rust
suite completed, sequential host rebuild and frontend checks passed. No process
was killed, deadline changed or production retry added for that build collision.

Evidence: artifacts/hub-panel-virtual-{check,focused,focused-confirmed,evidence,
evidence-confirmed,native-focused,rust,clippy,msrv,contract,node,schema,host,
host-confirmed,nina,net48}.log. No hardware or installed vendor driver was used.
Preceding 36a5558 PR/push CI 37550182065/37550174221 now both pass all eight jobs,
including Windows packaging/installer acceptance. Reviewed native/import/virtual
increments may now be pushed; their own CI is still required. Dedicated panel
simulation/shared creation and every original later gate remain required.

Push checkpoint: native panel output 554794f, registered COM input 593c0de and
virtual input 5a22737 are pushed together after both preceding 36a5558 CI runs
complete successfully. PR #21's description now reflects their verified scope.
New PR/push CI 37552662096/37552655796 is queued/running. Its native panel
cold/production registration and portable acceptance remain to be established.

### Dedicated panel simulation review (2026-10-06, local increment)

Reviewed the explicit V2 simulated source through the existing source actor,
typed controller, polling, virtual composition, IPC and all three outputs. Cover
motion and light readiness have independent monotonic clocks. Atomic sparse
updates replace only the selected component's pending operation; durations and
faults do not cancel either operation. Strict Int32 brightness/live maximum,
state bounds, independent completion and absent-component invariants are shared
with generated native/browser controls. No hardware transport or worker is added.

Eight panel cases cover atomic rejection, paused-clock independence, Halt/Off and
disconnect behavior, sparse updates, On(0), absence, live bounds, fault injection,
applied-write fencing, two-layer composition/provenance/age and actual IPC
revision rejection without saved configuration changes. All 35 simulator cases
pass. Actual Alpaca HTTP and real net48 x86/x64 clients verify nonblocking
independent operations, typed cache and retained uncertainty without automatic
actuator cleanup. Three new NINA cases cover completion, cancellation,
stopped-short/wrong-brightness results and the rendered sparse setup form.
Warnings-denied NINA 226/226, Node and seven independent schema checks pass.
The actual WPF capture is visually verified and documented in the setup guide.

The first simulator run failed an immediate lease-count assertion after a setup
update. SourceLease drop schedules release; the corrected fixture awaits the
observable zero count within its existing bound. Production ownership/timing is
unchanged. The original failure log is retained. A full Rust check also failed
to replace regain-alpaca.exe while the concurrently started NINA suite owned it.
NINA was allowed to finish normally; sequential confirmation passes the full
Rust hub/Alpaca suites, strict Clippy, Rust 1.89 all targets and generated-contract
freshness. Formatting and diff checks pass. No process was killed or test deadline
relaxed.

Evidence: artifacts/hub-panel-simulation-{check,focused,focused-confirmed,ipc,
http,node,schema,host,nina-focused,capture,nina,net48,rust,rust-confirmed,clippy,
msrv,contract-generate,contract}.log. Shared panel creation stays gated until its own review/apply/reload
acceptance. No equipment or installed vendor driver was activated; every
original later milestone gate remains required.

### Panel Automation CI review (2026-10-06)

Both 5a22737 CI runs 37552662096/37552655796 finish with seven successful jobs
and a Windows failure at the new panel DeviceState Automation assertion. All
19 registered parent/import cases pass. The export fixture requires the boxed
value's managed enum identity; an Object/VARIANT boundary can instead carry its
underlying Int32. Manual exports on this machine retain the enum and pass the
original assertion, so they do not reproduce that CI environment difference.
The original CI failures are retained in artifacts/hub-panel-publication-{pr,push}-ci-failure.log.

[Microsoft's object marshaling contract](https://learn.microsoft.com/en-us/dotnet/framework/interop/default-marshalling-for-objects)
maps an IConvertible Int32 value to VT_I4 and back to System.Int32. New real
net48 x86/x64 checks marshal actual CoverStatus and CalibratorStatus values to
native VARIANTs and verify tag 3 (VT_I4), Int32 type and unchanged value on return.
Existing managed DeviceState checks still require the declared ASCOM enums.
The Automation fixture now accepts only that declared Int32 enum or System.Int32
in 0..5; strings, Short, Double and unrelated enums remain rejected. Failures
report actual assembly-qualified types and values. Driver behavior, command
policy and time bounds are unchanged. The CI runtime type is not captured by
the original short assertion; new CI must confirm this wire-correct assertion
and complete the remaining registration/packaging checks.

Local confirmation passes warning-denied both-architecture staging, actual
net48 x86/x64 enum-to-native-VARIANT checks and eight-output manual COM exports
with both server and client bitnesses. Logs:
artifacts/hub-panel-ci-export-{build,reproduction,confirmed}.log and
artifacts/hub-panel-creation-net48-confirmed.log. Cold/production registration
remains a CI gate; no installed vendor driver was activated.

### Shared panel creation review (2026-10-06, local increment)

Reviewed reuse of generated source/output fields, runtime capability admission,
revisioned review/apply/reload, saved identity ledger, native selectors and COM
registration. CoverCalibrator has its own published output capability; COM and
simulation source classes use their existing transport/bitness gates. Cameras
remain disabled. No extra executable, transport or configuration definition is
introduced. Typed controller/output behavior is unchanged.

Existing native editor, actual WPF and real net48 creation fixtures now include
panels. They create two outputs for one simulated source, reject mismatched
classes before activation, retain saved identities/numbers and verify actual
cover completion, shared On(0) and independent leases. Actual browser acceptance
creates outputs 7/8, rejects a focuser/panel mismatch, preserves both output IDs
on save/reload and applies a cover-only state update without changing brightness,
light state or configuration revision. Observed status confirms zero leases and
disconnected transport. Native/browser screenshots are visually verified and
documented. Only the uniquely identified private browser fixture processes were
stopped afterward; no equipment or installed vendor driver was activated.

The first net48 run caught a stale expected proxy-choice list; it now requires
the newly supported panel choice while cameras remain disabled. A focused NINA
compile caught a test namespace qualification; global:: resolves the installed
NINA CoverState enum. Both original logs are retained. Confirmation passes full
Rust hub/Alpaca suites, strict Clippy, Rust 1.89 all targets, contract freshness,
Node/seven independent schema checks, NINA 228/228 and real net48 x86/x64 clients.
Formatting/diff checks pass. New CI for the preceding simulator/Automation fix
is still live; keep this reviewed creation increment local until it finishes.

Evidence: artifacts/hub-panel-creation-{contract-generate,node,schema,host,
focused,focused-confirmed,net48,net48-confirmed,rust,clippy,msrv,contract,nina}.log
and artifacts/hub-panel-creation-browser-{state,saved,verification}.json. Every
original remaining camera, coordination, recovery, documentation, acceptance
and final review gate remains required.

## 2026-10-06: camera image buffer and ImageBytes foundation

Reviewed the native core Frame/Session contracts, existing Alpaca camera image
stream, scalar actor/IPC limits and current ASCOM/Alpaca image specifications.
The new camera module separates binary images from bounded scalar polling and
does not enable any camera setup gate. Its acquisition requirements are recorded
in hub-contract.md; the source supervisor and frontend integration remain open.

Implemented validated geometry/encoding, immutable shared buffers and an atomic
payload-byte budget retained by the last reader. Consuming native adoption moves
the existing pixel allocation. Export performs order conversion in at most
64-KiB chunks, preserving plane order, signed/unsigned values and all nine numeric
encodings. Int32 packed as Byte/Int16/UInt16 is explicitly supported. Unknown
types/conversions, overflow, wrong transactions, malformed rank, partial bodies
and trailing data fail without publishing an image. Metadata/error bounds and
caller-owned deadlines/cancellation prevent unbounded response retention. The
codec never sends, retries or reconciles an equipment command. Host-wide staging
and conversion memory accounting is explicitly still required during integration.

Eleven private Rust tests pass, including competing allocations, reader lifetime,
native zero-copy adoption, non-square RGB/LRGB and one-plane rank-three arrays,
all numeric/packed encodings, multi-chunk order conversion, malformed metadata,
bounded UTF-8 upstream errors and cancellation during a partial download. The
existing actual Alpaca camera response test now decodes its production image
stream through the shared reader, preserving unsigned values 50000/65535.
Full Rust hub/Alpaca suites, strict Clippy, Rust 1.89 all-target checks, generated
contract freshness and formatting/diff checks pass. No frontend behavior or
configuration changed, and no hardware or installed driver was activated.

Retained initial checks: after review changed adoption to consume the native
frame, a test partially moved its exposure before adoption; cloning its small
exposure descriptor corrects the fixture. Clippy required is_multiple_of for
chunk alignment. The original failed logs and successful confirmations remain
in artifacts/hub-camera-image-{focused,focused-confirmed,rust,rust-confirmed,
clippy,clippy-confirmed,clippy-final,msrv,msrv-confirmed,contract}.log.
Current preceding panel CI 37554304962/37554298269 remains live with Windows
test.ps1 still running. Keep reviewed local increments until those runs end.
Next: source-owned acquisition supervision, binary backend/IPC paths and all
camera inputs/outputs, followed by every original remaining plan gate.

## 2026-10-06: fenced binary camera source dispatch

The existing source actor now dispatches binary image reads outside scalar
sampling. Admission requires a connection lease, exclusive control and an
explicit matching source generation. The caller supplies the shared image
budget and a separate positive deadline bounded to the existing core's one-hour
download limit; the one-second default scalar request timeout is not used.
Existing adapters return Unsupported without any image I/O until their camera
implementations are added. No capability or configuration choice is enabled.

Review covered queued and dispatched cancellation, current/old generations,
transport retirement, command ambiguity and memory lifetime. A queued abandoned
read is skipped. A dispatched read completes or times out within its own bound;
an undelivered image drops its reservation. Transport loss preserves the original
error and changes generation. Binary reads neither modify scalar cache nor replay
a command, create a write fence or clear an existing fence. The exclusive owner
may perform an explicit reconciliation read after uncertainty; publication still
requires the future acquisition supervisor to prove ownership. Detached captures
must also retain runtime activity so configuration quiescence cannot retire an
active acquisition; that requirement is recorded for supervisor integration.

Eight private actor tests and the full Rust hub/Alpaca suites pass. Tests include
observer rejection, pre-dispatch generation fencing, independent disconnects,
image readers surviving source shutdown, distinct image/scalar deadlines,
partial-download timeout/cancellation, cancelled queue entries, unsupported
existing adapters and retained uncertain writes. Strict Clippy, Rust 1.89 all
targets, generated-contract freshness and formatting/diff checks pass. Fresh-host
NINA passes 228/228; actual net48 x86/x64 regressions pass with zero build warnings.
No equipment or installed vendor driver was activated.

The first focused compile used unwrap_err on a task whose successful image has
no Debug implementation; the fixture now matches the cancellation error without
requiring a pixel debug representation. Initial and confirmed logs are retained
at artifacts/hub-camera-source-{check,focused,focused-confirmed,focused-final,
rust,clippy,clippy-final,msrv,msrv-final,contract,host,nina,net48}.log.
Preceding panel CI 37554304962/37554298269 has passed its Windows test script and
is still running installer acceptance. Keep local commits pending those terminal
results. Next: the source-owned acquisition supervisor, camera adapters and all
three publications, followed by every original remaining acceptance/final gate.

## 2026-10-06: panel CI confirmation and camera foundation publication

Exact preceding head 2f3f8c2 passes all eight jobs in both PR/push CI
37554304962/37554298269. Windows completes its test script, registered imports,
production exports, packaging and installer acceptance. This verifies the panel
Automation correction in CI; it does not establish a cause for unrelated older
connection/motion failures or replace physical/interactive acceptance.
Reviewed shared panel creation 1c67398, image foundation b154c87 and binary source
dispatch 56c2e7a now proceed together to the same draft PR. Their own new-head CI
remains required. Local validation includes full Rust hub/Alpaca, all eleven image
and eight binary actor cases, strict Clippy, Rust 1.89 all targets, generated
contracts, fresh-host NINA 228/228 and actual net48 x86/x64. Camera acquisition,
adapters, three outputs, coordination and every original final gate remain open.

## 2026-10-06: Rust 1.99 image CI correction

Both 3ed8515 runs 37556724162/37556718661 finish with five Rust/Windows lint
failures and three successful jobs. Rust 1.99 deprecates AtomicUsize.fetch_update
and requires fixed-size as_chunks access; local Rust 1.97 had passed. Logs are
retained in artifacts/hub-camera-foundation-{pr,push}-ci-failure.log.
The image budget now uses an equivalent checked compare_exchange_weak loop,
preserving its concurrent bound and Rust 1.89 support without suppressing lints.
Header/test parsing uses typed fixed-size chunks without redundant conversion.
All nineteen image/binary actor cases, explicit Rust 1.99 strict Clippy and
Rust 1.89 all-target checks pass. Initial typed-array fixture compile failure
and corrected confirmation are retained. No command, deadline or test assertion
was relaxed. New-head CI must confirm this correction.

The acquisition supervisor and shared runtime activity guard are under local
implementation and are not part of this CI correction. Their review/tests must
complete before publication. All original camera and later gates remain open.

## 2026-10-06: source-owned camera acquisition supervisor (local)

Reviewed admission, source-generation fencing, exclusive control, caller loss,
Stop/Abort semantics, immutable image ownership, optional timing and uncertainty.
The runtime activity guard is factored into a shared counter; integration must
use that same counter and one supervisor per source. No camera choice is enabled.

Nineteen private virtual-clock cases pass. They cover invalid geometry/duration,
independent observers, owner disconnect, cancelled preflight and dispatched
waiters, shortened Stop versus discarded Abort, unsupported/malformed capability
values, old readers pinning the memory budget, readiness/download deadlines,
generation loss after pixel copy, identity/geometry replacement and independent
optional metadata. Valid leap dates, fractional UTC timestamps and malformed
date/type cases are exercised through actual acquisition publication.

Review added explicit monitor notifications and synchronous control release
before successful publication/Abort completion. A 60-second polling fixture
proves immediate new admission after an acknowledged Abort without advancing
time. Cancelled command preflight restores monitoring and sends no Abort; an
already dispatched Abort completes even after its caller and session disappear.
Uncertain start/Abort retain ownership and do not replay or send automatic cleanup.

The first uncertain-Abort fixture expected the shared write fence to survive
after every source lease was dropped. Existing last-lease teardown deliberately
retires that epoch. The corrected fixture retains an independent observer and
verifies that administrative abandonment does not clear its source fence or
authorize another exposure. Production teardown behavior/deadlines are unchanged.
Initial expanded failure and corrected confirmations are retained at
artifacts/hub-camera-acquisition-{expanded,expanded-confirmed,final-focused}.log.

Full Rust hub/Alpaca, explicit Rust 1.99 strict Clippy, Rust 1.89 all-target and
generated-contract checks pass. A sequentially rebuilt host passes NINA 228/228
and actual net48 x86/x64 regressions with zero build warnings. Formatting/diff
checks pass. Logs: artifacts/hub-camera-acquisition-final-{focused,rust,clippy,
msrv,contract,host,nina,net48}.log. No equipment or installed vendor driver was
activated. Preceding f7cfbcd CI 37557708906/37557702178 remains live, with six jobs
passing in each run. Keep this reviewed increment local until those runs are
terminal. Runtime/adapters, binary frontend transport, native recovery metadata
and every original gate remain open.

## 2026-10-06: upstream Alpaca camera ImageBytes (local)

Seven actual loopback cases pass, including the real source actor and acquisition
supervisor sharing one immutable download between two independent clients.
The image client preserves protected headers, no redirects/retries and bounded
connection establishment; its overall deadline comes from binary source dispatch,
not the scalar timeout. A 1.2-second body succeeds with the unchanged default
one-second scalar deadline. Cancellation waits for an actual allocation before
dropping the reader, then verifies released memory and one HTTP request.

Malformed/truncated/trailing bodies, wrong transaction/media type, budget exhaustion,
non-camera reads, binary/JSON device errors and HTTP retry/redirect responses are
covered. Error classifications/codes are shared with scalar requests and arbitrary
upstream text is redacted. The first end-to-end fixture echoed a zero transaction
for PUT because it inspected only query parameters; production correctly rejected
that acknowledgement as uncertain. The fixture now reads the actual form body.
Initial and corrected logs: artifacts/hub-camera-alpaca-{check,focused,
focused-confirmed,focused-final}.log. Stream support adds futures-io/wasm-streams
to the lockfile without upgrading existing packages. Full Rust hub/Alpaca,
strict Rust 1.99 Clippy, Rust 1.89 all targets and generated-contract checks pass.
A freshly rebuilt host passes NINA 228/228 and actual net48 x86/x64 regressions
with zero build warnings. The final focused check also verifies successful JSON
images are explicitly gated without a second download. Logs:
artifacts/hub-camera-alpaca-{final-focused,rust,clippy,final-clippy,msrv,contract,
host,nina,net48}.log. Formatting/diff checks pass. New-head CI remains required.

Successful JSON images still need a bounded array decoder. No camera setup choice
is enabled and no equipment/vendor driver is used. All original camera/runtime,
recovery, coordination and final gates remain required.

## 2026-10-06: image CI result and panel failure diagnostic

f7cfbcd push CI 37557702178 finishes with seven successful jobs, including all
Rust 1.99 checks, and a Windows NINA failure in
ActualPanelPublisherAndNativeNinaShareLightWithoutOwningEachOthersLeases.
Its connected=true request returns 1280; the original assertion exposes only
the number. Original output is retained at
artifacts/hub-camera-image-ci-push-failure.log. PR CI 37557708906 has seven jobs
passing with Windows still live. Do not cancel/restart it or infer the failure's
cause from a later green run.

The fixture keeps the same zero-error requirement and deadlines but includes the
exact private response envelope, sourceStatus and the loopback server's existing
request trace on failure. Targeted local confirmation passes; this diagnostic is
not a proven production fix. Logs: artifacts/hub-panel-publisher-ci-diagnostic
and hub-panel-publisher-ci-diagnostic-final.log. Earlier intermittent Windows
connection/motion failures remain open for investigation.

## 2026-10-06: camera foundation CI confirmation and reviewed publication

Exact f7cfbcd PR CI 37557708906 finishes with all eight jobs successful, including
Windows tests, production registration, packaging and installer acceptance. Its
push CI 37557702178 fails the NINA panel connection case described above. The
green PR run verifies the Rust 1.99 lint correction but does not explain or fix
that intermittent failure. The diagnostic assertion is preserved in 457b3af.

Reviewed local acquisition supervision e243a05, panel diagnostic 457b3af and
upstream ImageBytes 119d71c proceed together to the same draft PR. Their full
local regressions pass; new-head CI remains required. JSON decoding is being
implemented separately and is not included in this reviewed publication.
Camera choices and every original remaining acceptance/final gate stay open.

## 2026-10-06: bounded JSON camera images (local)

Reviewed the fallback required by Alpaca content-type negotiation. One finite
JSON response is staged in reserved chunks; a shape/envelope pass precedes typed
decoding directly into one pixel allocation. Field order is irrelevant. No
serde_json::Value pixel tree is constructed. Raw chunks and final pixels share
the same payload budget; pinned readers are not evicted. Small device errors can
still report their original codes when that budget is fully pinned.

Ten private JSON stream cases, two cancellation unit cases and seven updated
actual HTTP cases pass. They verify all nine numeric encodings and signed/unsigned
extremes, negative zero, normal/subnormal floating limits, non-square RGB/LRGB and
rank-three one-plane data, strict ranges/types, duplicate fields/transactions,
malformed/ragged/empty/deep shapes and finite/trailing data. A response exceeding
one MiB decodes independently of scalar envelope limits. Actual source/supervisor
cases share one binary or JSON Double capture between two clients and retain its
pixels after source shutdown. No second download, exposure retry or vendor I/O
occurs. Admission/cancellation tests prove four waiting decoders, freed slots,
partial-staging cleanup and an abandoned queued blocking decode releasing memory
before allocating pixels.

Review corrections: use terminal ConnectionAborted instead of Interrupted in the
blocking reader, since std::io::Bytes retries interruptions; allocate working memory
only after decoder admission instead of embedding 64 KiB in every async future;
retain local allocation/decoder failure classifications rather than label them as
malformed upstream images. Numeric and string tokens, field count and raw payload
are bounded before parser work; primitive typed deserialization avoids integer
coercion or invented U16 packing. Errors have controlled text at the adapter.

A deterministic sample of 4,000 finite Double bit patterns reproduced a one-ULP
change at pixel 3 with default serde_json parsing. Enabling float_roundtrip fixes
that test; no lockfile or dependency version changes are required. Retain both
artifacts/hub-camera-json-float-fidelity.log and
artifacts/hub-camera-json-float-fidelity-confirmed.log. This sample does not prove
every possible floating bit pattern. Explicit yields bound work per async poll;
cancellation also aborts queued blocking work. Staging remains budgeted until a
cancelled queued task is consumed; this is not a whole-process RSS guarantee.

Initial/final focused logs: artifacts/hub-camera-json-{check,focused,
focused-confirmed,final-focused,cancellation,complete-focused}.log. Final full
Rust hub/Alpaca suites, Rust 1.99 Clippy, Rust 1.89, generated contracts,
rebuilt-host NINA 228/228 and actual net48 x86/x64 pass against all reviewed
changes. Nineteen core camera tests also pass with feature unification; 39 hub
unit tests include both cancellation regressions. Final logs:
artifacts/hub-camera-json-reviewed-{rust,clippy,msrv,contract,host,nina,net48,core}.log.
Camera choices stay gated. Runtime,
remaining adapters/properties/settings, native recovery metadata, all outputs and
every original later gate remain required. The latest ten JSON/seven actual HTTP
cases pass together (artifacts/hub-camera-json-complete-focused.log).

## 2026-10-06: terminal supervisor CI failures and local fixture evidence

1579a90 PR CI 37560177877 finishes with seven successes and Windows failure in
ActualPanelPublisherAndNativeNinaShareLightWithoutOwningEachOthersLeases. This
failure is a native structured read error after On(0), not the preceding HTTP
connection failure. The catch now retains the structured native error, private
source status, fixture state and upstream trace with the original inner exception.
The private fixture enqueued its command marker before changing start state;
tests could observe the marker and complete motion before that state was written.
Publish the marker after the start-state writes, still before any lost-reply stall.
This corrects a fixture ordering defect without proving it caused the CI failure.

Push CI 37560173015 finishes with six successes. Windows fails the queued-caller
cancellation test while writing a private peer reply (HubWire.WriteFrame); Intel
macOS fails a fresh nested-wheel Position read with a transport-lost Transient
error. Failure-only diagnostics retain peer frame/elapsed/client state and wheel
source snapshots/request trace. No deadline, expected result or command replay
policy changes. Retain artifacts/hub-camera-supervisor-{pr,push}-ci-failure.log.
All three causes remain unproved and reliability acceptance stays open.
The final full Rust/228 NINA/both net48 regressions also verify the changed
private fixtures locally. These passes do not prove the CI failures resolved.

## 2026-10-06: camera JSON publication and subsequent CI evidence

Reviewed JSON 07c0faa and fixture corrections 0923e51 are pushed to draft PR #21.
PR CI 37562038506 finishes with seven successes, including all four Rust platforms,
package verification and registered COM activation. Windows fails two NINA cases:
invalid-hello validation receives Timeout instead of Protocol, and panel HTTP
Connected=true returns 1280. The panel failure now retains the source snapshot
(connection re-entry with transportConnected=false) and private response writes
aborted with SocketException 10053. This is stronger evidence of connection loss,
but does not prove its root cause. Retain
artifacts/hub-camera-json-pr-ci-failure.log. Push 37562034286 remains live.

The private accessory HTTP fixture now sets NoDelay on accepted sockets: it sends
small headers/body separately and should not add Nagle/delayed-ACK latency to
ownership/deadline tests. No source deadline or success criterion changes. This
is fixture hardening, not a demonstrated explanation of the CI failures.

## 2026-10-06: shared camera properties, settings and owner cooling (local)

One property definition provides 53 typed camera members and polling plans;
thirteen typed setters reuse source leases, generation checks and the acquisition
supervisor. Booleans, Int32, finite numbers and budgeted strings/flat arrays keep
their types. Optional errors remain independent, with their upstream codes.
ImageReady and last-exposure timing use the immutable published acquisition;
external upstream buffer changes cannot change that image's identity.

Settings preflight preserves numeric/named-index Gain/Offset modes and live
bin/readout/capability limits. Only Unsupported permits a mode fallback. ROI
combination checks stay at StartExposure; setting one bin axis forwards one write
and leaves symmetric propagation to the source. No fabricated counterpart write,
rollback, exposure retry or successful-value cache is introduced.

Review found and corrected source-owned setter admission. The first focused run
passed 24/26: a second setter queued behind a held write and eventually saw a
retired generation; an existing uncertain fence was masked by a snapshot error.
A source-wide retained settings reservation now rejects concurrent setters and
starts before source queueing, and uncertainty takes priority for setters. Its
ID/owner/property are observable in shared status. Before-dispatch caller loss
skips writes; dispatched work retains activity/ownership after caller loss.
The original failure log remains artifacts/hub-camera-properties-focused.log.

Capture settings remain frozen, while CoolerOn/SetCCDTemperature can use the
capture owner's retained operation during exposure/readout. Siblings are blocked.
Publication and owner Stop/Abort do not race an outstanding cooling write. Dropping
its waiter retains work; completion wakes even a long-poll readiness monitor.
No borrowed operation release ends the exposure. A final acquisition-state check
prevents preflight from dispatching after a capture becomes uncertain or abandoned.
An initial deadline test reached the shorter serial-read deadline instead; its
corrected private timing gives the readiness deadline precedence without changing
production bounds. Retain both final-focused and final-confirmed logs.

All thirty focused cases pass: nineteen existing acquisition cases and eleven
new property/setting/cooling cases. They cover malformed/bounded values, exact
single writes, temporary ROI incompatibility, live modes/capabilities, old sessions,
frozen timing, both cancellation boundaries, concurrent settings/starts, owner
cooling, pending publication, late preflight and uncertain no-replay behavior.
Full final hub/Alpaca Rust, strict Rust 1.99 Clippy, Rust 1.89 all targets, generated
contracts and formatting pass. Fresh-host NINA 228/228 and real net48 x86/x64 also
pass, including the NoDelay private fixture change. Logs:
artifacts/hub-camera-properties-final-{confirmed,rust,clippy,
msrv,contract,host,nina,net48}.log. Earlier Clippy found one test-only needless
string conversion; it was removed without a lint suppression.

This layer is not yet wired into camera runtime, adapters, binary IPC or frontend
publication. Native recovery metadata and staging admission, all camera outputs,
coordination and every original acceptance/final gate stay open. Camera choices
remain disabled. Keep the increment local while preceding push CI remains live.

## 2026-10-06: native camera admission and immutable recovery metadata (local)

The preceding JSON push CI 37562034286 finishes with all eight jobs successful,
including Windows installer/release acceptance. Its separate PR failure remains
retained and unexplained. Reviewed properties/settings and private fixture NoDelay
hardening are pushed through ef0748e; PR/push CI 37564346322/37564341366 is live.
No failure was cancelled, restarted or converted into a relaxed assertion.

Review of the original native image helper found admission after allocation and
loss of Frame metadata. Its unshipped API is replaced by reserve_native followed
by consuming adoption. The actual core Session wrapper acquires the permit before
capture; one permit covers final pixels, the worker Vec during Vec-to-Arc conversion,
64 KiB of raw reply header and 128 KiB of retained encoded metadata. Admission is
one atomic reservation; error/cancellation drops it. Adoption validates the full
Exposure (including ROI origin, binning, duration and light/dark), moves the Arc and
releases staging. Cloned readers pin the same pixels, metadata and reservation.

Core image calls now reject above-ROI replies before allocating/reading pixels;
existing exact length/geometry checks remain. Reply parsing is factored for private
AsyncRead fixtures, preserves SDK error codes and retires protocol failures through
the same Worker call path. Result metadata moves out instead of cloning the Value
tree. Pixel allocation is fallible. Checked size multiplication rejects extreme
dimensions rather than overflowing. Generic and streaming worker calls retain
their existing size ceiling and error/cancellation behavior.

All encoded metadata survives adoption: native timestamps, actual backend/fallback,
replacement exposures, SDK and retained-frame retries, handle reopens, USB resets,
controls, white balance and cleanup evidence when present. A bounded writer rejects
metadata overflow instead of truncating or dropping fields. Its fixed 128-KiB
capacity remains charged for the final image. These are payload bounds, not decoded
Value-tree, whole-host RSS or worker-process memory accounting. No new recovery
attempt, deadline or exposure is introduced. The helper's caller must retain owned
capture work; frontend disconnect must not cancel the core token. A native backend
and its source-owned lifecycle are still required.

Validation: twelve image cases, three actual worker-simulation capture cases and
three private core reply cases pass. Tests prove a withheld oversized body is
rejected immediately, error codes/empty stream polls remain intact, all admitted
exposure fields match, metadata overflow frees its permit, and pixel adoption makes
no copy. SDK/direct simulation preserves metadata and two actual retained-frame
read recoveries. A last reader blocks a second capture before any exposure/recovery
event; releasing it permits the next capture. Cancellation before dispatch and
during an active exposure releases output and staging and preserves core's typed
Cancelled failure. Every equipment-facing test uses explicit simulation.

Full core/hub/Alpaca Rust, strict Rust 1.99 all-target Clippy, Rust 1.89 all-target
checks, generated-contract freshness and formatting pass. The added active-cancel
case also passes focused Clippy/MSRV after its addition. Fresh-host NINA 228/228 and
real net48 x86/x64 regressions pass. Evidence:
artifacts/hub-camera-native-admission-{focused,cancellation,rust,clippy,msrv,
test-clippy,test-msrv,contract,host,nina,net48}.log.

Next: native camera backend/configuration, retained lifecycle and recovery/cooling
allowances, one host budget/runtime supervisor per source, all image outputs and
remaining input adapters. Recovery metadata migration, coordination, conformance,
hardware/client acceptance, README/site documentation, main reconciliation and all
original final gates remain open. Camera choices stay disabled; keep this reviewed
increment local while ef0748e CI runs.

## 2026-10-06: retained native camera ownership (local)

The native owner serializes one core Session while exposing a separate readable
snapshot. Connection, capture and cleanup work retain their own Arc and runtime
activity; waiters own neither cancellation tokens nor tasks. Concurrent connects
join one handshake. Valid capture admission reserves payload memory and activity,
then publishes an operation ID and clears the prior image. Invalid/busy admission
preserves that image. Core recovery remains unchanged and no deadline/retry is
added around it. Immutable image readers continue to pin payload and metadata.

Explicit Abort cancels only the current owned core capture, discards its result
and acknowledges after cleanup. Idle Abort is inert; Stop is not fabricated.
Reset retires the generation synchronously, cancels pending core work and retains
cleanup before reconnect. Stale capture/connection completions and obsolete cleanup
cannot publish or close a replacement connection. Wait notifications are enabled
before checking state, preventing lost completion wakes. Unexpected task loss
withdraws connected state, reports a failure and clears its marker; reconnect
closes the prior session before opening. Normal source teardown must call close,
rather than relying on eventual Session/worker Drop for hardware cleanup.

Review tightened two races before final validation. Activity is reserved before
publishing any operation marker, including close/reset, so runtime replacement
cannot see an accepted operation without retained work. Acquisition identity is
checked separately from generation, preventing an old/unknown waiter from receiving
a newer capture's error. Connection/abort/close acknowledgements also recheck
their generation and do not report an unexpected task failure as success. Source
errors retain SDK codes with fixed text; raw worker diagnostics stay in core status.

Eight actual worker-simulation cases pass: joined/abandoned connection waiters,
dropped capture waiters with live status and frozen images, invalid/budget-rejected
admission preserving the prior frame, explicit SDK/direct Abort and fresh capture,
reset during capture and before dispatch, repeated reset, abandoned close, typed
SDK failure without replacement retries, and capture completion after loss of all
external owner references. The final two normal-capture tests use six-second
simulated exposures instead of a brief 200-ms phase window, without increasing
test deadlines. All eight confirmed cases pass; production bounds are unchanged.

Full core/hub/Alpaca Rust, strict Rust 1.99 all-target Clippy, Rust 1.89 all-target
checks, generated contracts and formatting pass. The final test-only duration
change also passes focused Clippy/MSRV. Fresh-host NINA 228/228 and real net48
x86/x64 regressions pass. Logs:
artifacts/hub-camera-native-owner-{check,focused,reviewed,confirmed,rust,clippy,
msrv,test-clippy,test-msrv,contract,host,nina,net48}.log.

The owner still needs the typed native source adapter, configuration/factory and
runtime sharing, recovery/cooling allowances and all frontend image outputs.
Inspection also found a required cooling refinement: Session.queue_control changes
desired values, but capture read_environment only reads temperature/power; direct
server get/set rejects capture-time writes except cached environmental reads.
Implement a common acknowledged in-capture cooling path, retaining target changes
across recovery. Do not claim queued intent is hardware application or bypass the
worker's capture ownership. This is original scope, not an optional follow-up.
Camera creation remains gated and all original later gates remain open.

Current ef0748e PR/push CI 37564346322/37564341366 both finish with all eight
jobs successful, including Windows packaging/installer checks. This does not
prove NoDelay or marker ordering explains the earlier intermittent failures.
Native admission and ownership can now proceed to their own CI together with
the reviewed direct cooling increment after local validation.

## 2026-10-06: acknowledged direct-worker cooling (reviewed; CI required)

The direct worker now admits only target/enable writes during capture through one
bounded request slot. The existing USB owner executes requests at its environment
service checkpoints, without another USB handle or concurrent transfer thread.
An acknowledgement follows application, environment service and readback;
cached active getters expose the acknowledged target/enable alongside temperature
and power. Imaging and auxiliary writes remain excluded during capture; hardware
capability, writability and range checks still precede admission.

Unsent expiry never reaches USB. A dispatched timeout retains its slot until the
owner finishes; failures or missed acknowledgements fence later cooler writes.
Owner retirement wakes queued waiters as unsent and dispatched waiters as
uncertain. Review corrected late completion after retirement, so it cannot turn
published uncertainty into a known acknowledgement. Capacity is released before
known completion is published. No locks are held over USB operations.

The framed error envelope preserves control uncertainty even through hardware
error context. Core maps it to UncertainControl, retires the worker and marks it
non-retryable, including SDK codes that would otherwise permit capture recovery.
Native ownership maps it to an Uncertain source error with redacted text and the
original code. This primitive does not supply Session's common acknowledgement
queue or preserve changed live targets through frozen recovery settings yet.

Further review found that core's outer command deadline could precede the direct
worker's uncertainty reply and leave a cooler write retryable. Worker exchange
now records framed write admission. Lost/malformed replies, outer timeout and
cancellation after that point become typed non-retryable uncertainty and retire
the process. Cancellation before dispatch remains Cancelled without consuming
a command ID. An actual SDK-simulation worker parked in download verifies both
timeout and post-dispatch cancellation, non-retryability and process exit. The
cancellation test polls through dispatch instead of guessing with a sleep. The
cooling slot uses one completion timestamp for fencing and publication, avoiding
a deadline boundary between those decisions.

Four queue cases cover known acknowledgement, before-dispatch expiry, retained
dispatched timeout, owner loss, USB failure, fencing and late completion after
retirement. Private Host tests exercise still/video exposure and retained-frame
cooling, unchanged exact pixels, unsupported cameras, invalid values and excluded
imaging/auxiliary controls. A real production worker-process framing case runs
six-second still/video simulations with live acknowledgements and exact pixels.
The core error parser separately verifies typed non-retryable uncertainty and
retained codes. No physical camera or installed vendor driver is activated.

Full Rust core/hub/Alpaca/ZWO regressions pass. After the final retirement review,
all 96 ZWO library tests and all 25 core tests pass with a freshly rebuilt worker.
The first worker build reported an unused wait_until wrapper after simulation
moved to the existing service callback; the wrapper is now test-only. Final
strict Rust 1.99 Clippy, Rust 1.89 all-target checks, generated contracts and
formatting pass. After the outer-deadline correction, freshly rebuilt-host NINA
228/228 and actual net48 x86/x64 clients pass with no build warnings.
Logs: artifacts/hub-camera-direct-cooling-{focused,worker,rust,worker-reviewed,
reviewed,core,outer-deadline,final-worker,final-rust,final-clippy,final-msrv,
final-zwo,final-host,final-nina,final-net48,contract}.log.
Both preceding ef0748e CI runs pass all eight jobs; this reviewed increment and
the two native camera increments proceed to their own CI on the same draft PR.
Keep every original camera/runtime/output/recovery,
coordination and final acceptance gate open; camera creation remains disabled.

## 2026-10-06: common acknowledged Session cooling (local)

One bounded mailbox now serves SDK/direct cooler target and enable changes on
Session's existing worker owner. Submit validates capabilities without changing
shared values. Queued and claimed/preflight requests can expire or lose their
receipt without dispatch. Dispatched commands retain ownership after receipt
loss; uncertain outcomes fence new admission, invalidate the worker and are
non-retryable by capture recovery. Known completion commits shared/applied values
and the active recovery map before publishing its receipt. Only cooler keys are
live in otherwise frozen capture/replacement settings; frame control metadata
records the final acknowledged values rather than a thermal history.

Review separated claimed/preflight ownership from worker dispatch. It also
carries one absolute deadline through write/readback, capped by the configured
command timeout, and checks it immediately before framed write admission. An
unsent expiry preserves worker/framing and consumes no command ID. Cancelling a
waiter after dispatch cannot free capacity or turn a late outcome into success.
Identity checks prevent retired completions from modifying a replacement request.
Close, invalidation and Drop retire queued/dispatched receipts with distinct
certainty. No mutex is held over I/O. Existing serialized worker ownership and
core exposure/download retry limits remain in force.

Six mailbox cases cover bounded/validated admission, commit-before-receipt,
queue/preflight expiry and caller loss, owned dispatched completion, timeout
fencing, retirement/reactivation, stale results and retained upstream codes.
Production worker simulations verify SDK target changes survive a replacement
exposure while imaging settings remain frozen, direct target changes survive
worker recovery, idle acknowledgement/teardown, readback mismatch without target
publication or capture retry, and live target/disable commands during recovery
settle. An expired worker command leaves the same process usable for valid framed
commands. No physical devices or installed vendor drivers are activated.

Full core/hub/Alpaca regressions and strict Rust 1.99 Clippy/Rust 1.89 all-target
checks pass. Final core confirmation includes all 37 tests; the added settle case
also passes and its test-only changes pass focused strict Clippy/MSRV. Generated
contracts, formatting, rebuilt-host NINA 228/228 and actual net48 x86/x64 clients
pass with no build warnings.
Logs: artifacts/hub-camera-core-cooling-{mailbox,worker,focused,reviewed-worker,
core,rust,clippy,msrv,final-core,final-clippy,final-msrv,contract,host,nina,net48}.log.
NativeCamera still needs a retained command task for idle/capture, generation and
activity ownership, error/publication fencing and typed source adapter integration.
The legacy queue_control API remains deferred desired intent. Do not report it
as hardware acknowledgement. Camera creation stays disabled; every original
camera/output/recovery/coordination and final acceptance gate remains required.

Pushed native admission/owner/direct-cooling head 6584e67 PR/push CI
37568068870/37568064681 both now finish with all eight jobs successful, including
Windows packaging/installer acceptance. Earlier intermittent connection/motion
failure causes remain unproved. Session cooling and the retained NativeCamera
integration below pass local validation and require their own CI.

## 2026-10-06: retained NativeCamera cooling (reviewed locally; CI required)

NativeCamera now owns a separate cooler command marker, receipt, generation,
token and runtime activity. Its caller only owns a cancellation guard: leaving
before dispatch withdraws the queued/claimed request, while dispatched work stays
owned. Admission is synchronous under source state, reserves activity before
publishing markers and rejects concurrent commands/capture/abort. The outer
supervisor remains responsible for client/source lease authorization.

The command task waits for either the receipt serviced by capture or idle engine
access. It never drops a dispatched service future when the receipt expires; idle
worker retirement stays owned. No second worker/handle or parallel core call is
introduced. Reset/close cancel old work and fence completions synchronously;
generation checks also reject a known ACK buffered before its caller resumes.
Pending cooling pauses image publication, including waits on already completed
images. Unknown outcomes preserve a redacted Uncertain error and upstream code,
cancel active capture and prevent later capture completion from overwriting the
fence. New captures, commands and connection shortcuts cannot clear uncertainty;
explicit reset/close is required. Existing readers keep immutable pixels.

Review also prevents unexpected capture-task loss from replacing an existing
uncertainty fence, and prevents an idle cooler task from servicing a disconnected
or uncertain source. Unsent expiry is Transient without transport_lost. Receipt
cancellation checks request identity, so an old guard cannot cancel a later slot.

Fourteen production-worker simulation owner cases pass (six new), including idle
SDK/direct acknowledgement, validation/bounded admission, queued caller loss,
live six-second SDK/direct capture, unchanged exact pixels/final cooler metadata,
publication waiting, reset before dispatch/after buffered ACK, idle/capture
readback mismatch, no retries and explicit reconciliation. Three owner unit cases
verify queued expiry with a held engine, caller loss after owner ACK and final
external-reference loss, redaction/code preservation and expiry classification.
A seventh core mailbox case verifies the separate caller cancellation handle
before claim, during preflight, after dispatch and against a later slot.

Retain the initial 12/14 owner run: both failures were fixture setup errors. The
clamp rejected the initial capture target, so the fixture now acknowledges zero
before capture and fails only the intended later negative request. Also retain
the terminated paused-clock expiry runs and diagnostic log. Advancing exactly to
the timer deadline left it pending; advancing one millisecond beyond the timer
tick resolves it. Temporary stage prints were removed and the next real command
uses the ordinary timeout. No production deadline or success criterion changed.
The first strict Rust 1.99 Clippy run rejects a nested conditional; the collapsed
condition preserves behavior and passes final strict linting and owner tests.

Full Rust core/hub/Alpaca regressions pass, including all 38 core tests and final
three owner unit cases. Strict Rust 1.99 Clippy, Rust 1.89 all-target checks,
generated contracts, formatting, freshly rebuilt-host NINA 228/228 and actual
net48 x86/x64 clients pass with no build warnings. Logs are under
artifacts/hub-camera-native-cooling-*.log. No physical equipment or installed
vendor drivers were activated. Native typed properties/settings, adapters/config/
runtime, host budget/recovery allowances, binary frontend IPC, all camera outputs,
coordination and every original acceptance/final gate remain open. Camera creation
stays disabled; these owner primitives alone are not frontend camera support.

## 2026-10-06: native typed properties and desired geometry

Reviewed native RAW16 mapping against the shared 53-property type/validation
contract, core sensor/control capabilities and immutable completed-frame metadata.
Missing controls and malformed types/ranges remain Unsupported or Unavailable;
there are no fabricated zero/calibration/progress values. Cooler enable requires
an exact Boolean control value. Bayer offsets reflect the four actual patterns;
monochrome cameras reject Bayer properties. Optional core-absent properties remain
Unsupported. Native StopExposure/asymmetric binning are not advertised.

Connection now validates dimensions/bins and refreshes initial controls and
environment before publishing connected state. Failure retains cleanup ownership.
Private clamp fixtures initially rejected that new initial refresh: retain the
12/14 failure log. Their minimum is now -10, allowing the initial target, while
the later -15 request still proves mismatch/uncertainty without replay.

Desired geometry has private fields and no deserialization bypass. Initial
geometry selects an advertised bin rather than assuming bin one. Individual
setters preserve intermediate ROI combinations and do not silently replace ROI
on bin changes. Combined bounds/alignment are validated at StartExposure under
the same state lock that freezes geometry and reserves memory. Busy/uncertain
setters cannot change it. Hardware settings cannot use this local-only API.
Duration and UTC start are taken from the successful immutable core frame, not
next-capture settings or an invented frontend timestamp. Invalid/missing UTC
fails independently of known duration. Cached property reads remain usable during
capture, but adapter integration must preserve their true observation age.

Four property unit cases and eighteen production-worker simulation owner cases
pass, including SDK/cooled direct and uncooled direct cameras, all shared property
types, geometry admission, frozen readers/timing, absent controls, malformed sensor
metadata and retained uncertainty. Full core/hub/Alpaca Rust regressions, strict
Rust 1.99 Clippy, Rust 1.89 all-target checks, generated-contract freshness,
formatting, Node checks, seven independent schema checks, rebuilt-host NINA
228/228 and actual net48 x86/x64 fixtures pass with no build warnings. The first
schema command used system Python without jsonschema; the existing private schema
venv passes. Logs: artifacts/hub-camera-native-properties-*.log. No physical
equipment or installed vendor drivers were activated.

Retained a92b8bd CI failures: push 37571649524 finishes with seven successes and
a Windows initial focuser connection-reply failure. Its trace records an aborted
header write; the cause is unproved. Shared fixture failure tracing now includes
elapsed time, without changes to production code, deadlines, retries or expected
outcomes. PR 37571654100 fails macOS at an immediate source-lease-count assertion
after native-wheel disconnect. SourceLease release is retained/asynchronous; the
fixture now waits for cleanup using the existing three-second test bound. The
targeted wheel regression, NINA and both net48 clients pass. The PR's Windows job
remains live at installer checks; preserve it and require new CI for these changes.
CI logs: artifacts/hub-camera-native-cooling-ci-push-failed.log and
artifacts/hub-camera-native-cooling-ci-pr-macos-job.log.

Native acknowledged gain/offset commands, remaining adapters/config/runtime,
host-wide recovery/budget wiring, binary frontend IPC, all camera outputs,
coordination, documentation and every original acceptance/final gate remain open.
Camera creation stays disabled and PR #21 stays draft.

## 2026-10-06: acknowledged native gain/offset commands

Reviewed idle imaging settings against the existing core persistent controls,
SDK offset clamp policy, cooler mailbox, capture recovery and native task/generation
ownership. Core gain/offset and cooler commands now share one write/readback
helper and absolute deadline. Gain, offset, cooler target and cooler enable use
the same framed-write tracking. A lost/malformed/timed-out/cancelled dispatched
write is uncertain and non-retryable; unsent expiry remains distinct. Only
write-plus-readback success updates applied/shared values. Recovery restores those
actual values. SDK offset may retain the existing bounded clamp policy and returns
the actual applied value; other mismatches/out-of-range readbacks retire the worker.
Review added a final deadline check so an immediately ready late reply cannot
become success. The original cooler receipt/expiry semantics remain unchanged.

NativeCamera admits one Configuring operation and activity before dispatch. A
queued caller loss/expiry skips engine I/O and preserves previous image/state.
Once admitted to the engine, write, readback and retirement are owned independently
of the frontend future. Pending settings block captures, other settings, Abort
and image publication. Review found that waits on an older acquisition also need
to check this pending marker; they now wait for the known setting outcome just
like new image reads. Unknown outcomes preserve a redacted source fence/code
until explicit reset/close; unexpected configuring-task loss is also uncertain.
Reset rejects late completions and already-buffered acknowledgements. Known
settings preserve the preceding immutable image and its capture metadata/timing.
The outer supervisor still authorizes source/control leases.

Four new core cases pass: SDK/direct readback and worker recovery; gain mismatch
without publication/retry; bounded SDK offset clamp versus out-of-range readback;
and no-I/O capability/cancellation/expiry/cooler-conflict preflight. Existing
worker fault cases now cover all four persistent controls for dispatched deadline
and cancellation, plus unsent expiry with unchanged process/framing/command ID.
Three new native owner integration cases cover conflicting admission, old-reader
publication/timing, reset/uncertainty reconciliation, buffered ACK fencing and
capture-time imaging-write rejection. Two new owner unit cases verify queued
caller loss/expiry and dropping the actual frontend future synchronously after
the write ACK but before readback, including loss of all external owner references.
That deterministic hook uses a useful core diagnostic distinguishing write ACK
from pending readback; it introduces no timing sleeps or physical equipment.

Full core/hub/Alpaca Rust regressions pass, including 42 core tests, 48 hub unit
tests, 21 native owner integration cases and all existing camera/accessory/runtime
suites. Strict Rust 1.99 Clippy, Rust 1.89 all-target compatibility, generated
contracts, formatting, Node/seven independent schema checks and rebuilt-host
NINA 228/228 pass. The first strict Clippy run identifies a test MutexGuard inside
a polling macro; the corrected explicit poll closure releases it before returning
and all five native owner unit cases pass again. Real net48 x86/x64 production-host
fixtures pass with no build warnings. Logs: artifacts/hub-camera-imaging-control-*.log.

Preceding a92b8bd PR CI 37571654100 is now terminal with seven successes and the
retained macOS asynchronous-wheel-cleanup assertion failure. Its Windows job
passes packaging/installer/release checks. Push 37571649524 retains seven successes
and its initial focuser connection failure; the successful PR does not explain
that cause. The reviewed local wheel cleanup correction and failure timing
diagnostic still require new CI. The full PR failure log is retained at
artifacts/hub-camera-native-cooling-ci-pr-failed.log.

Next: native source adapter/config/factory/runtime with host-wide image budget,
runtime activity, recovery allowances and truthful observation ages. Neither
cached reads nor a queued desired value can be presented as new hardware evidence.
Remaining camera inputs, binary frontend IPC, all three camera outputs, recovery
metadata migration, coordination, documentation and every original acceptance/
final gate remain open. Camera setup choices stay disabled; no hardware or
installed vendor driver was activated and PR #21 remains draft.

## 2026-10-06: worker/core/native observation freshness

Reviewed direct environment sampling/publication, SDK reads, production worker
framing, desired versus acknowledged core state, native cached properties,
capture-time cooling and replacement-worker ownership. `get-observation` has a
strict integer-value/relative-age envelope; existing `get` replies and command
restrictions are unchanged. Direct temperature, regulator output power, accepted
target and enable have independent evidence times. Publication copies those
times rather than replacing them with the cache-read/publication time. During
still/video captures and retained frames, observations use the existing owner
cache without queueing USB work behind an exposure. Cooler changes update only
their affected values; they cannot freshen unrelated temperature/power samples.
Review also corrected output power publication to follow successful USB writes,
so a failed write cannot become acknowledged demand.

Core normalizes the relative age against request admission, conservatively
including all request/response time without comparing process clock epochs.
Review removed legacy queued gain/offset/cooler apply's duplicate write/readback
path: these four controls use the same acknowledged helper/deadline and uncertain
outcome retirement as retained commands. A dispatched invalid readback can no
longer enter capture recovery and repeat a persistent control write. Other legacy
controls retain their existing paths and are outside this timestamped contract.
Negative, nonfinite, overflowing or malformed ages fail. `Status.observations`
stores acknowledged values/times separately from desired `values`; it is skipped
in JSON. Queueing desired settings cannot publish or refresh evidence. Apply and
retained write/readback commit evidence only on success; cooling still commits
through its receipt's final ownership/deadline check. Invalid age after a write
is uncertain and retires the worker without new evidence or replay. Unavailable
worker diagnostics may retain aged evidence, but opening a replacement clears
it before capability negotiation and restoration. Native property reads now use
acknowledged evidence, never desired values. Their observation API preserves the
original time, rejects future/missing evidence and distinguishes host-local state
or negotiated metadata by an absent hardware timestamp. Cached reads do no I/O.

An important protocol distinction is now explicit: direct gain/offset readback
acknowledges accepted next-capture worker configuration. Actual sensor-register
programming occurs at exposure start, and per-capture overrides/immutable frame
metadata remain separate. SDK observation ages timestamp the SDK call, without
claiming knowledge of the vendor's internal caching. Continuous streaming keeps
its pre-existing legacy-command exclusions, including the new observation call.

Tests cover independent aged direct telemetry during six-second still/video
captures and retained frames, strict SDK control parsing, unchanged legacy reply
shapes, production worker pipes for all six controls, conservative transit-time
normalization, desired-value isolation, replacement-worker clearing, JSON clock
exclusion, no-I/O native cached reads, unrelated-control timestamp preservation,
and old evidence retained after uncertain readback. A simulation-only SDK reply
override exercises aged/invalid evidence through the actual process pipe; it
cannot run against a physical SDK. Initial test compile errors (a misplaced
variable and unnecessary Clone), an unused close Result, and the reply override's
incorrect pixels identifier were corrected before final validation.
The first aged-pipe assertion compared internal request admission to an earlier
external clock as an upper bound. It now checks both actual admission interval
boundaries; the exact age and pure transit-time assertion are unchanged. All four
queued persistent controls additionally exercise invalid age, one write ACK,
retired worker, preserved evidence and no reconnect/replacement exposure.
The first queued-enable fixture requested its already-applied value, correctly
causing no write or readback. It now disables the initially enabled simulated
cooler and explicitly asserts that every fixture requests a changed setting.
The default Python lacked jsonschema; the existing hub-schema-venv runs all seven independent
schema checks successfully. Keep the original logs alongside corrected runs.

Final combined core/hub/Alpaca/ZWO Rust regressions pass, including 47 core tests,
49 hub unit tests, 22 native owner integration cases and 98 ZWO library tests.
Strict Rust 1.99 Clippy, Rust 1.89 all-target checks, generated-contract freshness,
formatting/diff checks, Node/seven independent schema checks, rebuilt-host NINA
228/228 and actual net48 x86/x64 fixtures pass. Main evidence:
artifacts/hub-camera-observation-core-rust-complete.log,
artifacts/hub-camera-observation-core-clippy-complete.log,
artifacts/hub-camera-observation-core-msrv-final.log,
artifacts/hub-camera-observation-core-contract.log,
artifacts/hub-camera-observation-core-nina-reviewed.log and
artifacts/hub-camera-observation-core-net48-reviewed.log. Earlier failed runs
remain alongside these final results. Preceding adfb9e2 PR/push CI
37575030689/37575027240 both pass all eight jobs, including Windows installer and
release checks; that success does not prove the earlier Windows failure's cause.
This observation checkpoint requires new CI and is not frontend acceptance.
Next: native camera source adapter,
configuration/factory/runtime, shared host budget/activity/recovery allowances
and preserving these ages through SampleBatch. Remaining inputs, binary frontend
IPC, all camera outputs, coordination and all original acceptance/final gates
remain open. Camera choices stay disabled; PR #21 remains draft and no physical
equipment or installed vendor driver was activated.

## 2026-10-06: native camera source actor/supervisor integration

NativeCameraBackend implements the common Backend lifecycle using one retained
NativeCamera. Construction does no discovery/I/O. Incremental handshake steps
leave the source actor responsive; the owner keeps connection/cleanup work after
waiter loss. Simulation identity comes from the actual core Runtime. Scalar
reads use the existing 53 typed properties; setters share strict member/parameter
decoding for the 13 settings. Known malformed commands fail before dispatch.
StartExposure requires bounded Duration/Light, converts to native microseconds
and freezes configured geometry atomically. Sub-microsecond/zero native captures
are invalid; fractional native temperature targets are rejected rather than
silently truncated. StopExposure remains unsupported, never an Abort alias.

Sampling uses the common plan/type/aggregate bounds. Optional-property failures
stay per key. Hardware samples retain their original evidence ages through the
actual SourceActor; local geometry/state and negotiated metadata carry no hardware
timestamp. A read-only core environment method uses only the existing worker,
reads temperature/power and cannot apply queued settings or implicitly reopen.
Failure retires the worker while retaining aged diagnostic evidence. The owner
retains refresh activity and fences generations; it skips capture/setting work.
Adapter commands await a running refresh before dispatch. An empty sample plan
does not schedule telemetry. Explicit Refresh remains supported.

Review found that sharing the ordinary pending marker with background telemetry
could race ImageReady and camera_image, hiding an already immutable image.
Refreshing is now excluded from publication fences and reports idle camera state;
capture/setting/cooling/cleanup reservations keep their original fences. The
image, metadata and budget charge remain unchanged through telemetry. Binary
image dispatch verifies that native owner and caller use the exact same shared
ImageBudget and returns the existing Arc without allocation or another download.

Review also found a handshake replay risk: initialization acknowledges persistent
settings, so a timeout/reset or invalid readback cannot automatically reopen and
replay them. The adapter retains a conservative unknown-outcome fence through
automatic resets. Only complete last-lease disconnect clears it. SourceActor
publishes the normal write-uncertain latch for uncertain connection outcomes;
typed source waiters return Uncertain promptly. No generic read-only reconnect
behavior for other existing adapters was removed.

Further review/integration tests exposed the distinction between a logical core
Session and its current worker handle. Recovery and explicit Abort retire a worker;
the first adapter incorrectly interpreted that as a source transport failure,
changing generation after a known Abort. Cached reads now follow the owner's
logical connection. Background telemetry skips an absent worker without reopening.
Core continues its own configured recovery with the same acquisition/generation.
A later explicit hardware control validates its requested range/capability first,
then retains restoration of acknowledged settings before the requested setting's
normal write/readback. Local geometry and StartExposure do not trigger a separate
restoration; core capture owns its own restore. Reset while connecting, configuring
or cooling conservatively retains the outcome fence, including restoration tasks.
The integrated SDK/direct case now verifies owner-only cooling, unsupported Stop,
known Abort, subsequent explicit gain restoration and a new capture with unchanged
source generation. The injected failed-status case reads cached properties during
core reconnect delay and aborts without an outer reset or replacement exposure.
Another test resets control restoration before dispatch and proves the requested
gain was never applied; a complete disconnect clears that conservative fence.

Production worker-pipe simulations cover SDK/direct captures through actual
SourceActor/CameraSupervisor, two clients/one connection/one capture, shared pixel
identity, frozen native metadata, pinned readers after last disconnect, preserved
aged temperature evidence in SampleBatch/actor caches, optional errors, malformed
commands/no capture, foreign-budget rejection/no allocation, lost initialization
outcomes/no automatic replay, explicit disconnect/reconnect, and refresh/capture/
reset/publication races. Two core cases prove queued settings are not applied by
telemetry and failed telemetry cannot reopen the worker.
Eleven adapter integration cases pass, with production simulations only.

Retained initial failures: the test helper double-wrapped SourceHandle's Arc;
an aged-gain initialization override correctly failed write/readback freshness
validation. The aged-cache fixture now uses read-only temperature evidence;
production validation is unchanged. The first reconnect-delay fixture guessed
the phase name as "SDK reconnect delay"; the actual core phase is "Reconnect
delay" and the fixture now observes that actual state. The first Abort integration
failure exposed the worker/logical-session bug described above. A later explicit
restoration fixture used the default five-second reconnect delay against a
five-second source deadline, correctly yielding uncertainty. It now uses the
same explicit 0.01-second simulated reconnect delay as other owner fixtures;
the source deadline and failure semantics were not relaxed. Production factory
recovery/deadline derivation remains required. The broad unrestricted build hit Windows
paging-file exhaustion (os error 1455), followed by compiler metadata errors.
The same broad build succeeds with two compiler jobs. Final validation following
the publication and worker/logical-session corrections passes: full combined
core/hub/Alpaca/ZWO regressions (49 core tests, 49 hub unit tests, 22 native owner
cases, eleven new adapter cases and 98 ZWO library tests, plus all integrations),
strict Rust 1.99 Clippy for core/hub/Alpaca/ZWO/device all targets, Rust 1.89
all-target compatibility, generated contract freshness, Node/seven independent
schema checks, formatting/diff checks, freshly rebuilt-host NINA 228/228 and
actual net48 x86/x64 integration fixtures. Main evidence:
artifacts/hub-camera-native-source-rust-complete.log,
artifacts/hub-camera-native-source-clippy-complete.log,
artifacts/hub-camera-native-source-msrv-complete.log,
artifacts/hub-camera-native-source-contract.log,
artifacts/hub-camera-native-source-node.log,
artifacts/hub-camera-native-source-schema.log,
artifacts/hub-camera-native-source-host-final.log,
artifacts/hub-camera-native-source-nina-final.log and
artifacts/hub-camera-native-source-net48-final.log.
Earlier failed runs remain alongside these final results. Code review covered
generation/operation guards, retained cleanup/activity, pre-dispatch validation,
read/write uncertainty, publication through telemetry, budget identity, original
sample ages and core-owned worker recovery. New CI is required before acceptance.

Preceding observation CI b038f8e is terminal: PR 37578062435 and push
37578058561 each pass seven jobs and fail one Windows NINA case. PR fails second
focuser initial connection, push fails shared panel read. Private upstream writes
report SocketException 10053; root cause remains unproved. Logs and job metadata
are saved as artifacts/hub-camera-observation-{pr,push}-ci*. The private fixture
now records request-parse elapsed time, response-write start and thread-pool counts
so future failures can distinguish scheduling/parse delay from reply writes.
This adds evidence only; no deadlines/retries/assertions or production paths
changed. Prior green runs do not prove these failures' cause.

Next: native factory/config/runtime, host-wide budget/activity and native recovery
allowances, other camera inputs, bounded binary frontend IPC and all three camera
outputs. Discovery/config transfer, recovery metadata migration, OS resume,
coordination, conformance, physical/interactive acceptance, README/site updates,
main reconciliation and the original final audit remain required. Camera choices
remain disabled and PR #21 stays draft. Only explicit simulations/private fixtures
were activated.

## 2026-10-06: canonical native recovery metadata and camera configuration

Core recovery options now declare their fourteen serialized keys, typed defaults,
ranges, units and descriptions once. The macro supplies the existing value type,
validation and strict frontend schema data, using only existing serde/JSON
dependencies. Root and model module reexports remain compatible. Four independent
legacy-format tests pin the shipped default JSON, sparse overrides/unknown
extensions, integer types/bounds and all positive/nonfinite floating boundaries.
Validation still classifies invalid values as Failure::Invalid. Strictly positive
fields keep their original range down to tiny positive values; no new UI floor
changes saved policy behavior. The core schema is explicitly for strict frontend
configuration; it does not change the tolerant legacy profile loader.

Hub CameraRecovery rejects unknown keys but retains the core options/defaults.
NativeCameraConfig adds an exact model, explicit direct-only SDK fallback and
native recovery under SourceBackend::Native.camera. Native camera sources require
it; accessory sources reject it. Alpaca/COM remain separate strict variants and
do not gain native retry promises. Schema conditionals enforce camera class,
presence and fallback constraints, independently checked by Python. Semantic
errors use full source/camera/recovery field paths. Selection carries the source's
exact serial and backend without discovery, launch or hardware I/O. Six config
cases cover sparse selection, typo/type rejection, semantic paths/nonfinite
values, class constraints, atomic persistence and duplicate direct/SDK claims.
The older duplicate-claim regression now supplies otherwise-valid camera settings.
Existing accessory fixtures serialize identically because absent camera settings
are omitted. Native accessory construction also rejects class-mismatched camera
settings when called outside the complete config validator.

Review covered serialized compatibility, defaults, strict versus legacy loading,
positive/inclusive bounds, backend/serial selection, source identity/polling
preservation and no new recovery behavior. It corrected an overbroad deadline
description to name gain/offset/cooler write-readback behavior. Node and native
NINA readers verify shared defaults, descriptions, units and exclusive lower
bounds. Real net48 x86/x64 editor clients also read the live host's recovery
metadata. Camera choices remain capability gated.

The initial full NINA run ended 228/229: publisher/native switch sharing received
ASCOM ValueNotSet (1026) immediately after an acknowledged SetValue. Inspection
found the source actor intentionally invalidates all cached samples after writes,
then schedules a confirming poll. A new deterministic Rust test holds that poll,
proves both clients get Unavailable instead of old/optimistic values, releases it
and observes the confirmed value with exactly one write. The publisher fixture
now treats only that post-ACK ValueNotSet/unavailable response as pending, still
requires the exact confirmed value, retains its original deadline, rejects all
other errors and never retries a write. This corrects the fixture's completion
model; production cache/error/deadline behavior is unchanged. It does not explain
the distinct earlier Windows COM/HTTP connection/reply failures. The original
failed log remains artifacts/hub-camera-recovery-nina-final.log.

Final validation passes: full combined Rust core/hub/Alpaca/ZWO regressions,
four core recovery compatibility cases, six camera config cases, sixteen existing
config cases and all seven switch cases; strict Rust 1.99 Clippy for core/hub/
Alpaca/ZWO/device all targets; Rust 1.89 all-target checks; generated contract
freshness; Node/eight independent schema checks; formatting/diff checks; fresh
host NINA 229/229 and real net48 x86/x64 integration. Key evidence is
artifacts/hub-camera-recovery-rust.log,
artifacts/hub-camera-recovery-{core,config}-final.log,
artifacts/hub-camera-recovery-switch-{transition,complete}.log,
artifacts/hub-camera-recovery-clippy-complete.log,
artifacts/hub-camera-recovery-msrv.log,
artifacts/hub-camera-recovery-contract-check.log,
artifacts/hub-camera-recovery-{node,schema}.log,
artifacts/hub-camera-recovery-host-final.log and
artifacts/hub-camera-recovery-{nina,net48}-complete.log.

Preceding native adapter a9b3c50 PR/push CI 37581976334/37581971979 are still live.
Both have seven successful jobs and successful Windows test.ps1/Python tests;
Windows build/installer stages remain open. Keep this increment local until those
runs finish, then publish to the same draft PR. No physical equipment or installed
vendor driver was activated.

Next: native factory/runtime, shared host budget/activity across config revisions,
full core-derived recovery/connection/control allowances, other camera inputs,
bounded frontend image IPC and all three camera outputs. Discovery/config transfer,
OS resume, coordinated groups, conformance/interactive/physical acceptance,
README/site updates, main reconciliation and the original final audit remain
required. This checkpoint does not close milestone 4's full migration/runtime or
camera acceptance gate, and does not narrow the plan.

## Native camera factory and host resources (2026-10-07)

The factory now builds native SDK/direct camera backends from the source's exact
model/serial/backend/recovery selection and the host's explicit camera runtime.
Construction performs no discovery, worker launch, SDK load or pixel allocation.
Missing host resources fail Unsupported. A production SDK/fallback path must be
absolute; supplying a fixture without explicit native simulation fails closed.
Accessory fixtures explicitly omit camera resources and retain their previous
behavior. Typed camera proxy poll plans deduplicate all 53 properties and apply
the same combined sample bound as the other device classes.

CameraResources clones share the image-budget and activity Arcs across applied
revisions. The host builder supplies them once, and HubRuntime uses that activity
counter for output admission/quiescence as well as retained native work. The
registry's image dispatch still verifies budget identity. Review followed leases,
retained capture tasks, pinned readers, reservation shrink/drop, caller loss and
builder closure ownership. Source polling configuration is not rewritten. This
increment does not derive recovery timing or change core retries/deadlines.

Four factory cases exercise inert construction with absent worker/SDK paths,
required resources, explicit simulation, typed poll deduplication with runtime
camera admission still rejected, and SDK/direct production-pipe capture with two
clients. The capture case retains a pinned image across a new configuration
revision, fills the remaining shared budget, proves Busy before another exposure
dispatch, then releases capacity and captures without damaging the pinned image.
Its SDK exposure is deliberately non-instant so the retained activity assertion
does not depend on scheduling before a completed instant capture.

The actual executable fixture uses --hub-host --simulate --sdk with an absent
library, maps CCD temperature to an existing read-only Switch gauge, and verifies
two client leases, confirmed scalar values and independent disconnect. Setup
still omits nativeCameraSources. A CLI rejection case prevents --hub-attach from
overriding the SDK. Camera output and setup gates remain closed.

Focused factory/executable checks, strict Rust 1.99 Clippy, Rust 1.89 all-target
checks, Node/eight independent schema checks, fresh-host NINA 229/229 and real
net48 x86/x64 pass. The default Python invocation lacked jsonschema; the existing
private hub-schema-venv passes without an environment change. The first combined
Rust invocation stopped during build with Windows error 5 deleting the executable
while managed fixtures used it. That build did not run the full suite; its log is
retained. Managed fixtures completed successfully before the serialized Rust
confirmation began. Full core/hub/Alpaca/ZWO Rust regressions and generated
contract freshness now pass. Formatting and diff checks pass as well.

Evidence: artifacts/hub-camera-factory-final.log,
artifacts/hub-camera-runtime-{host,build,clippy,msrv,node,schema-venv,nina,net48}.log.
The original failed build is artifacts/hub-camera-runtime-rust.log; its serialized
confirmation is artifacts/hub-camera-runtime-rust-complete.log.

Validation commands (all final invocations pass):

```text
cargo build -j2 -p regain-alpaca -p regain-device --locked
cargo test -j2 -p regain-core -p regain-hub -p regain-alpaca -p regain-zwo --locked
cargo +1.99.0 clippy -j2 -p regain-core -p regain-hub -p regain-alpaca -p regain-zwo -p regain-device --all-targets --locked -- -D warnings
cargo +1.89.0 check -j2 -p regain-core -p regain-hub -p regain-alpaca -p regain-zwo -p regain-device --all-targets --locked
cargo run -j2 -p regain-hub --example export_config --locked -- contracts/hub-config.json --check
node scripts/test-hub-config.mjs
artifacts/hub-schema-venv/Scripts/python.exe scripts/hub/test_schema.py
dotnet test tests/Regain.NINA.Tests -c Release -warnaserror
scripts/test-hub-dotnet.ps1
cargo fmt --all --check
git diff --check
```

Preceding a9b3c50 PR/push CI both finished all eight jobs green. Recovery metadata
6f29557 is pushed to draft PR #21; runs 37585153377/37585149444 remain live. Keep
this factory increment local until those jobs finish. Next: full core-derived
connection/control/readiness/cleanup allowances and runtime camera supervision,
then remaining inputs, bounded binary frontend IPC, all camera publications,
coordination and every original acceptance/documentation/final gate. No physical
equipment or installed vendor driver was activated; no original milestone closes.

## Private HTTP scheduler isolation after recovery CI (2026-10-07)

Recovery head 6f29557 PR/push CI 37585153377/37585149444 are terminal with seven
successful jobs and one failed Windows job each. PR NINA ends 228/229: the panel
sharing fixture gets a transient calibrator-state read. Transaction 73 records
692 ms before parsing/reply start, then an aborted write at 719 ms. Its source
request deadline remains 300 ms. Push NINA ends 226/229: first focuser connection
has an uncertain connection write, rotator initial connection fails, and ETA
expects [590,500,510] but reads [590,600,510] after timer cancellation. Focuser
connection transaction 3 reports a reply-write failure at 53 ms; this trace
does not measure time queued before accept. Do not attribute every failure to
one cause or erase their original evidence.

The private HubAccessoryServer previously used shared thread-pool continuations
for acceptance, parsing and replies. The same managed tests exercise synchronous
native getters on that pool. A new isolated net48 child caps and occupies every
shared worker, then makes eight synchronous HTTP requests from its main thread.
The original fixture, compiled from ebcb704 into a private baseline project,
reproduces a socket read timeout. The replacement passes in x86/x64 and drains
an accepted partial request while every shared worker remains occupied. Limits
are changed only in that disposable child, never in the test runner or product.

The replacement uses one dedicated accept task, four dedicated request tasks
and a bounded queue of 64 clients. Synchronous socket operations remain on those
dedicated threads. The deliberate lost-reply delay is still one second and is
cancelled on disposal; fixture/source deadlines and assertions are unchanged.
All clients are tracked before queue admission, cancelled accept is joined before
cleanup, partial/queued clients close, worker tasks drain, and repeated disposal
is harmless. Unexpected protocol errors still fail the fixture. Review covered
queue saturation, shutdown races, EOF and disposed sockets, worker ownership,
global pool restoration and independent net48 process/bitness checks.

Full NINA 229/229 and real net48 x86/x64 pass after scheduler isolation. The
final partial-client acceptance barrier and failure cleanup also pass in the
same complete suites. Earlier full Rust, strict Clippy/MSRV and contract
checks remain valid: this refinement changes only managed private test fixtures
and their script. The first baseline compile failed an unused fault-field warning;
explicitly selecting normal replies removes that warning and the warning-denied
baseline then reproduces the expected HTTP failure. No production code changed.

Evidence: artifacts/hub-camera-recovery-ci-pr-windows-job.log,
artifacts/hub-camera-recovery-ci-push-windows-correct.log,
artifacts/hub-http-scheduler-baseline-build-final.log,
artifacts/hub-http-scheduler-baseline/stderr.log,
artifacts/hub-http-scheduler-nina-final.log and
artifacts/hub-http-scheduler-net48-complete.log. The initially misidentified push
job log is an ARM job and is not used as Windows evidence.

New CI is required. The HTTP dependency is reproduced and removed; the ETA
cancellation timing and distinct connection failures are not declared resolved
by a local pass. All original acceptance gates remain required. Continue native
core-derived timing and cleanup, runtime camera supervision, remaining camera
inputs/outputs and coordination after publishing these reviewed increments to
the same draft PR. No physical equipment or installed vendor driver was used.
