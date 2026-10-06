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
