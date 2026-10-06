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
