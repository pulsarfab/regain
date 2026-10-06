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
