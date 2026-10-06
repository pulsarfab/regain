# Hub review record

This records local review and tests for the single hub PR. Passing a foundation
test does not imply that a frontend, transport, or hardware gate has passed.

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

Next: corrected COM runner validation, native ASCOM output/registration, remaining
setup initialization, credentials, inspection/simulation/diagnostics, interactive
acceptance and every original broader proxy, camera, coordination and release gate.

Next: resolve Windows fixture activation, complete native ASCOM outputs/setup,
interactive NINA/vendor acceptance and shared setup refinements, then all original
broader proxy, camera/acquisition, coordination, conformance, recovery, hardware,
README/site/screenshots and final merge gates. PR #21 remains draft.
