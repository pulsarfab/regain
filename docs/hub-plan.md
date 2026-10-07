# Regain Hub implementation plan

Status: implementation in progress on `codex/regain-hub`; one PR against main.
Maintenance releases remain on `release/0.5`. Do not merge the hub PR until all
milestone gates and the final completion audit pass.
This branch uses development version `0.6.0` / Windows `0.6.0.0`; it has not been
tagged or published. Changed cross-crate APIs require an unpublished version so
Cargo package verification uses the new workspace packages.
Last updated: 2026-10-07.

Current increment: bounded Rust frontend image IPC is implemented and locally
reviewed/validated. A separate authenticated image stream
borrows an existing control client's output lease and pins its exact completed
source/generation/acquisition. It issues no equipment command. The host reserves
one reusable 64-KiB scratch buffer before acknowledgement, streams a finite
ImageBytes body and closes the stream. The Rust reader validates the manifest
and binary descriptor before publishing a budgeted immutable image. Five binary
codec/scratch/deadline cases and eleven runtime cases pass, including a real
private OS endpoint. Final hub/Alpaca regressions, all 69 hub unit tests, strict
Rust 1.99 lint, Rust 1.89 compatibility, formatting/contracts and Node/eight
schema checks pass. NINA 230/230 and actual net48 x86/x64 pass with the added
wire capability. IPC forbids binary metadata extensions or wrong transactions
before allocation; ordinary HTTP extension support remains unchanged. Full host
budget rejection, receiver rejection, reader EOF and stalled-writer deadlines
release transfer resources without replay. These tests use only simulations and
private fixtures.
Managed image readers, frontend operation timing, remaining camera inputs and
all three camera publications remain required, alongside the original gates.

Runtime/scalar camera output commit edc5a59 is pushed to draft PR #21. Its PR
37596918826 and push 37596912932 workflows are live. Keep subsequent work local
until those runs finish; no complete milestone or merge/release gate closes here.
Push CI's macOS Intel job 112711683350 has failed a nested filter-wheel
moving-position read with a Transient timeout; the same PR job passes. Both
Windows jobs remain live. The failed job's raw log is retained. The test now
reports elapsed read time, source snapshots, private requests and writes at that
assertion; no deadline, expected result or retry behavior is changed. Cause is
not established, and fresh CI is required before accepting this correction.
The diagnostic refinement passes all 22 filter-wheel cases locally, with strict
lint and Rust 1.89 checks; this does not prove macOS CI acceptance or a root cause.

Current checkpoint: camera outputs now share the runtime's acquisition supervisor
by source UUID. Runtime connections and typed scalar IPC cover camera properties,
settings, Start/Stop/Abort and owner-only abandonment of an uncertain acquisition.
Cached DeviceState and paged diagnostics transfer no pixels and perform no
equipment reads. Web/native diagnostic readers validate saved source, revision,
generation, acquisition ownership and image-readiness fences. Eight focused
runtime/real-pipe simulation cases and web checks pass. Full hub/Alpaca Rust
regressions, strict Clippy (installed stable and Rust 1.99), Rust 1.89 all-target
compatibility, generated contracts, eight schema checks, NINA 230/230 and actual
net48 x86/x64 pass after review. Clippy caught the acquisition diagnostic's large
enum variant; boxing that field preserves its wire schema and final focused
runtime checks pass. Public camera setup choices and
Alpaca/NINA/ASCOM camera publication remain gated; bounded binary image IPC,
frontend operation timing, remaining camera inputs, coordination and every
original acceptance/documentation/final gate remain required.

Both preceding 0ba47ec workflows are now terminal and green: PR 37592319880 and
push 37592313062 each pass all eight jobs, including Windows. This proves the
pushed retirement/timing and portable-fixture corrections. It does not cover the
newer runtime, contract-check and camera-output increments. Maintenance
remains on release/0.5; draft PR #21 remains the single hub PR for main.

Contract-check refinement: the correct export invocation includes
`contracts/hub-config.json --check`. A verification command supplied only
`--check`, which the exporter treated as a filename; its successful exit was not
a freshness check. The correctly invoked checker passes. The exporter now rejects
that missing-path form without writing a file. Actual CLI checks prove fresh
acceptance and stale rejection; strict Clippy, Rust 1.89 and formatting pass.
The original mistaken-command log is retained and its generated file removed.

Current runtime increment: HubRuntime owns one inert acquisition supervisor per
camera source UUID. Camera resources are selected before controller construction;
native actors expose their existing budget/counter identities, and both must
match the host. Injected registries adopt native resources instead of inventing
new accounting. Proxy-only embedding hosts can explicitly retain resources across
revisions without SDK settings. Cached acquisition status does no equipment I/O.
Runtime shutdown releases supervisor ownership and its image cache only after
the source actor stops and drains; external pinned images remain charged. Live
uncertainty cannot be abandoned through this retirement path, and its error is
retained after shutdown. Five focused cases pass, including SDK/direct actual
worker simulations, owner loss with an observer, capacity across revisions,
mixed-resource rejection, inert proxy hosts and uncertain retirement. Final full
hub/Alpaca regressions, strict Clippy, Rust 1.89, formatting/contracts, Node/eight
schema checks, NINA 229/229 and actual net48 x86/x64 pass after review refinements.
The first fixture incorrectly expected an image to survive every lease being
disconnected into a new generation; corrected coverage keeps the observer alive.
Camera output connections, bounded frontend image IPC, remaining input adapters,
all three camera publications and every original remaining gate are still open.
Preceding retirement/timing increments are pushed through 0ba47ec to PR #21;
PR/push CI 37592319880/37592313062 each have seven successes with only Windows
still running. Neither run is accepted as green yet. Keep new work local
until those runs finish.
The reviewed runtime increment is committed locally at 74bea3a.

Current timing increment: core now derives validated native connection, control,
capture and cleanup allowances from the existing recovery workflow. Canonical
persistent/acknowledged controls and fixed USB/close/fixture timings are shared
with execution. Replacement eligibility uses the same microsecond duration and
inclusive limit as core capture; long exposures acquire no replacement retries.
Native actors and supervisors use these allowances without changing scalar
read deadlines or granting retry policy to proxies. Generated polling descriptions
explain the native exceptions; stored keys, defaults and ranges are unchanged.
Five policy boundary cases and production-pipe connection, same-frame reread,
post-Abort restoration and held-host cleanup cases pass. A direct replacement
also restores gain/cooling and collects three stable samples before publishing
one shared image. Full Rust regressions, strict Clippy, Rust 1.89, generated
contracts and Node/eight schema checks pass, as do NINA 229/229 and actual net48
x86/x64. Review also fixes respecting a longer saved source connection allowance;
its inert constructor case and final hub/Alpaca regressions pass. Final strict
Clippy, Rust 1.89, formatting/contracts, NINA 229/229 and net48 x86/x64 pass after
that correction; evidence uses artifacts/hub-camera-timing-*-final.log.
Camera outputs remain disabled; all original remaining milestones and acceptance
gates still apply. Retirement/worker-path correction is committed locally at
d428f6e; both reviewed increments are published through 0ba47ec for fresh CI.

Current work: native camera retirement and portable host-fixture correction.
Factory/fixture commits ebcb704/edbafa7 are pushed to draft PR #21. At edbafa7,
PR/push CI 37587543962/37587539552 fail portable Rust jobs. Downloaded Linux,
Linux ARM and macOS PR logs identify the actual problem: the native-camera host
test searches target/debug for regain-device while portable CI builds workers
in target/release. The test now passes the existing REGAIN_TEST_WORKERS setting
explicitly, checks that worker before launch and retains source status on timeout.
All ten host cases pass locally using a separate worker directory with spaces.
Both runs are now terminal with four successful jobs and four portable failures.
Both Windows jobs pass. All eight downloaded portable job logs identify the
same missing debug worker path; neither run is accepted as green.

Native source shutdown now joins retained camera tasks before publishing actor
completion. A per-camera counter includes obsolete generations and adapter
connection waiters without waiting for unrelated host work. Disconnect timeouts
retain uncertainty; draining does not retry an ambiguous command. A connection
waiter reserves retirement before spawning and skips opening when disconnected
before its first poll. Core clears its worker PID only after retirement. The
actual host test holds its endpoint lock through blocked cleanup and cancelled
shutdown waiters, then releases it after retirement with the original uncertainty.
Full core/hub/Alpaca/ZWO regressions, the final focused host case, strict Clippy,
Rust 1.89, formatting, generated contracts, Node/eight schema checks, NINA 229/229
and actual net48 x86/x64 all pass locally. Evidence is recorded in hub-review.md.
The retirement correction is locally committed at d428f6e; preceding CI has
finished as recorded above. Its worker-directory fix still needs fresh CI.
This does not finish derived recovery timing or enable camera outputs.
All original remaining milestones and final acceptance gates remain required.

Recovery head 6f29557 CI has finished: PR/push runs 37585153377/37585149444 each
pass seven jobs and fail Windows NINA. PR fails a shared panel read; push fails
focuser/rotator initial connections and an ETA cancellation position assertion.
The panel trace records 692 ms before request parsing against the unchanged
300 ms deadline. An isolated fully occupied thread-pool test reproduces an HTTP
timeout in the original private fixture. Dedicated, bounded fixture workers
pass that check in real net48 x86/x64 and the local NINA suite passes 229/229.
This proves the fixture scheduler dependency, not every earlier CI cause. ETA's
timer-based assumption and the other distinct failures require fresh evidence;
no production deadlines, assertions or retries are weakened. Factory integration
is locally committed at ebcb704. Final NINA 229/229, x86/x64 scheduler isolation
and complete net48 client checks pass. Factory/fixture increments were published
together through edbafa7 to the same draft PR; its CI is recorded above.
All original gates remain open.

Native factory/resource integration is locally reviewed and validated. The production
factory now constructs configured native SDK/direct owners with one host-owned
image budget and activity counter shared across revisions. Focused production
simulation tests verify inert construction, explicit simulation, shared images,
retained-reader accounting and rejection before exposure when capacity is full.
The actual executable also passes a simulated native camera temperature gauge
with two shared client leases and an absent SDK library. Camera output admission
and setup choices remain gated. Full core/hub/Alpaca/ZWO regressions, strict
Clippy, Rust 1.89, generated contracts, Node/eight schema checks, fresh-host NINA
229/229 and real net48 x86/x64 pass. Core-derived
connection/control/readiness/cleanup allowances, remaining camera
inputs and all image publications remain next. No original milestone gate closes
at this checkpoint. Preceding a9b3c50 PR/push CI both finish all eight jobs green;
reviewed recovery configuration 6f29557 CI failures and the fixture refinement
are recorded above.

Native recovery configuration checkpoint (pushed, reviewed and validated): core
now declares all fourteen saved recovery keys, defaults, ranges and descriptions
once and generates schema data without a schema dependency. Legacy sparse
profiles and unknown-extension loading remain compatible; the hub uses a strict
wrapper to reject misspelled recovery fields. Native sources have class-specific
camera model/recovery configuration, explicit direct-only SDK fallback and exact
serial selection. Atomic persistence retains source/output identities and polling
settings. Structural schema enforces camera presence/class/fallback constraints;
semantic errors retain shared field paths. Web/native readers consume the same
metadata. Four legacy compatibility cases, six camera config cases, all seven
switch cases, the full Rust regressions, strict Clippy, Rust 1.89, generated
contracts, Node/eight schema checks, fresh-host NINA 229/229 and actual net48
x86/x64 pass. The first NINA run failed a post-write switch read with ValueNotSet;
a deterministic held-poll test proves the deliberate cache invalidation interval.
The fixture now awaits confirmed readback within the unchanged deadline and does
not retry commands or accept other errors. Earlier HTTP/connection CI failure
causes remain unproved. Preceding a9b3c50 PR/push CI both finish all eight jobs
successfully. Recovery configuration is pushed at 6f29557; its CI is recorded
above. Factory/host resources are now locally integrated as recorded above;
runtime acquisition supervision, derived recovery allowances,
remaining camera inputs and all image outputs are still required. Camera choices
remain disabled; no original milestone or final acceptance gate is closed here.

Latest local native camera adapter checkpoint: NativeCameraBackend now connects
the retained owner to the real SourceActor and CameraSupervisor. It shares strict
typed property/setting decoding, incremental connection steps, client/control
leases, independent hardware ages and immutable image storage. Image dispatch
rejects a different host budget without allocating or copying pixels. Read-only
idle telemetry never applies desired settings or opens a replacement worker.
Its retained activity/generation reservation skips captures/settings; commands
await it before dispatch, while completed images remain readable throughout.
Unknown initialization outcomes survive automatic reset/reconnect attempts until
the last client disconnects; typed connection waiters return that uncertainty.
Core worker retirement during recovery/Abort does not change the logical source
generation. A later explicit control can retain restoration of known settings;
background polling never triggers that restoration. Reset during setting/cooling
or restoration work retains the unknown-outcome fence. All eleven focused
production-pipe simulation checks pass. Full core/hub/Alpaca/ZWO regressions,
strict Rust 1.99 Clippy, Rust 1.89 all-target compatibility, generated contracts,
Node/seven schema checks, fresh-host NINA 228/228 and real net48 x86/x64 pass.
Final evidence is recorded in hub-review.md and
artifacts/hub-camera-native-source-*.log. Factory/config/runtime and host-wide budget/activity/recovery
allowance wiring remain next, alongside remaining camera inputs and all outputs.
Camera choices stay disabled; the original milestones and final gates stay open.

Observation checkpoint b038f8e CI has finished: both PR/push runs
37578062435/37578058561 pass seven jobs and fail Windows NINA tests. PR fails a
second focuser initial connection; push fails the shared panel read. Private HTTP
reply writes report SocketException 10053. Cause is unproved. Original logs are
retained; the fixture now records parsing/reply-start times and thread-pool counts
to distinguish scheduling from reply-write delay without changing deadlines,
retries, assertions or production behavior. Fresh CI is required.

Latest local camera checkpoint: timestamped worker observations now reach core
and native typed property reads. Core keeps acknowledged value/time evidence
separate from queued desired values. Temperature, power, target and enable retain
independent ages across cached reads and capture; unrelated setting writes do not
freshen them. Pipe messages transfer relative ages, conservatively including IPC
time, never process-local clock epochs. Replacement workers clear old evidence.
Direct gain/offset acknowledgements describe accepted next-capture configuration;
sensor programming occurs at exposure start. They are not sensor-register probes.
Full core/hub/Alpaca/ZWO Rust regressions pass, including 47 core tests, 49 hub
unit tests, 22 native owner integration cases and 98 ZWO library tests. Strict
Clippy, Rust 1.89 all-target checks, generated contracts, Node/seven schema
checks, rebuilt-host NINA 228/228 and real net48 x86/x64 pass. Legacy queued apply
for the four acknowledged controls now reuses the same helper and uncertain
outcome retirement; its invalid-readback tests prove no replay/replacement
exposure. Verification commands, initial fixture errors and their corrections
are retained in artifacts/hub-camera-observation-*.log and hub-review.md.
The terminal observation CI failures are recorded above; acceptance stays open.

Native properties/geometry fa04eb4 and acknowledged imaging controls adfb9e2 are
pushed to the same draft PR. Their complete local checks include full Rust,
strict Clippy/MSRV/contracts, Node/seven schema checks, fresh-host NINA 228/228
and real net48 x86/x64 fixtures. Both adfb9e2 PR/push CI runs
37575030689/37575027240 now pass all eight jobs, including Windows installer and
release checks. Their success does not establish the earlier Windows failure's
cause. Adapters/config/runtime,
recovery/budget wiring, binary frontend IPC, all camera outputs, coordination and
every original final gate remain.
Camera setup choices stay disabled.

Retained cooler checkpoint a92b8bd CI: push 37571649524 finishes with seven
successes and a Windows initial focuser connection-reply failure. PR 37571654100
finishes with seven successes and a macOS native-wheel lease-count failure.
The wheel fixture now waits for bounded asynchronous lease cleanup rather than
asserting immediate scheduler ordering. The Windows cause is unproved; shared
failure tracing adds elapsed time without changing deadlines, retries or success
criteria. The focused wheel case, NINA 228/228 and net48 x86/x64 pass locally;
both corrections require new CI evidence. Windows installer/release validation
passes in the terminal PR run; that pass does not explain the separate push failure.

Native wheel metadata checkpoint 630b302 passes all eight jobs in both PR/push CI
37536023962/37536015902, including Windows registered imports, packaging and
installer acceptance. Reviewed wheel polling/runtime increments are pushed at
08913c8. Push CI 37539201271 failed Windows initial focuser connection and an
Intel macOS rotator cleanup assertion; PR 37539206029 passes all eight jobs.
Verified Alpaca/native wheel publication at 0669339 passes all eight jobs in both
PR/push CI 37541898392/37541893308, including private cold/production wheel
registration and packaging/installer checks. Reviewed COM/virtual wheel inputs
are pushed at a586c76; all eight jobs pass in both PR/push CI
37544747351/37544741219. Wheel simulation/shared creation are pushed at 5d0ed34;
Both PR/push CI 37547703480/37547695748 pass all eight jobs, including Windows
installer acceptance. Reviewed panel controller/runtime/HTTP increments are
pushed at 36a5558; PR/push CI 37550182065/37550174221 both pass all eight jobs. Private
registered activation does not establish interactive/hardware acceptance.

Reviewed native panel outputs (554794f), Windows COM imports (593c0de) and virtual
inputs (5a22737) are now pushed together to PR #21. New PR/push CI
37552662096/37552655796 both finish with seven successful jobs and a Windows
failure in the panel Automation DeviceState fixture. Registered panel imports
pass. The fixture incorrectly requires a managed enum identity across VARIANT;
native Int32 enum marshaling is now checked explicitly in both net48 clients.
The Automation assertion admits only the declared enum or bounded Int32; driver
behavior is unchanged. Simulation and the correction are pushed at 2f3f8c2;
PR/push CI 37554304962/37554298269 both pass all eight jobs, including Windows
registered imports, production registration, packaging and installer acceptance.
Shared panel creation and the first camera image/source increments are pushed
through f7cfbcd. Their latest CI outcome and the locally reviewed acquisition
supervisor/upstream ImageBytes transport are recorded below.

Camera foundation head 3ed8515 PR/push CI 37556724162/37556718661 both finish with
five Rust/Windows lint failures and three successful jobs. CI uses Rust 1.99,
which deprecates fetch_update and requires fixed-size as_chunks access. The
MSRV-compatible checked CAS correction passes all nineteen camera image/binary
actor cases, explicit Rust 1.99 strict Clippy and Rust 1.89 all-target checks.
Replacement f7cfbcd push CI 37557702178 finishes with a Windows NINA panel-sharing
connection failure (1280); the Rust 1.99 lint correction passes. PR CI 37557708906
finishes with all eight jobs successful, including Windows installer acceptance.
This green run does not establish the cause of the panel failure. Its original
assertion omitted its error message; the local fixture now retains the exact
envelope, source status and upstream trace without changing success criteria or
deadlines. Its targeted local confirmation passes; the CI cause remains unproved.
Camera acquisition supervision is reviewed and committed locally at e243a05;
camera choices remain gated and every original remaining gate stays open.

Reviewed acquisition/ImageBytes/diagnostic commits are now pushed through 1579a90.
PR CI 37560177877 finishes with seven successes and a Windows native panel-sharing
read failure after On(0). Push CI 37560173015 finishes with six successes, a Windows
private peer reply deadline and an Intel macOS fresh nested-wheel read failure.
Their causes remain unproved. Failure-only native/peer/wheel diagnostics are pushed;
the panel fixture also publishes its command marker after setting its start state,
preventing the handler from overwriting a test's completed state. This ordering
defect is evident in the fixture; it is not proved to explain the CI failure.
The bounded JSON image decoder and diagnostic corrections are pushed through
0923e51. Ten stream cases and two cancellation unit cases
pass, with seven updated actual HTTP cases including binary and JSON Double
captures shared by independent clients. Raw response chunks and final pixels
compete for one budget; decoding does not construct a nested pixel Value tree.
All nine numeric types preserve their declared encoding/range, with strict shape,
rank, transaction and token bounds. Parsing is cancellable and limited to four
admitted decoders. A 4,000-pattern Double fidelity test reproduced a one-ULP parser
error; enabling serde_json float_roundtrip fixes it without dependency upgrades.
The final focused ten JSON/seven HTTP checks, full Rust hub/Alpaca suites, nineteen
core camera tests with feature unification, Rust 1.99 strict Clippy, Rust 1.89,
contract freshness, rebuilt-host NINA 228/228 and real net48 x86/x64 pass against
the reviewed JSON changes. PR CI 37562038506 finishes with seven successes and
Windows failure: invalid-hello validation receives Timeout instead of Protocol,
and panel HTTP connection fails with 1280. The panel trace now shows aborted
private response writes and connection re-entry; the root cause remains unproved.
Push CI 37562034286 has finished with all eight jobs successful, including Windows
installer/release checks. This does not explain the separate PR failure. Camera runtime,
remaining adapters, all three outputs, native recovery metadata
and every original later gate remain open.

Camera properties/settings checkpoint (pushed at ef0748e): the existing source supervisor now
shares 53 typed properties and 13 setters, preserving optional errors, numeric and
named gain/offset modes, range/capability checks and frozen publication timing.
Thirty focused cases pass (eleven new cases plus nineteen acquisition cases).
A source-wide retained settings reservation rejects concurrent setters/starts
before they queue behind a dispatched write. Before-dispatch caller loss skips
the write; after-dispatch loss retains ownership/activity until its known outcome.
Capture settings remain frozen. The capture owner can adjust cooling while
exposing/reading through its retained lease; sibling cooling writes are rejected.
Publication waits for an outstanding owner cooling write. A deadline during
preflight prevents a late write; uncertain writes retain the source fence.
Pending setting identity/owner/property is available in shared status. Full final
Rust hub/Alpaca, strict Rust 1.99 Clippy, Rust 1.89, contracts, rebuilt-host NINA
228/228 and real net48 x86/x64 pass. Reviewed properties/settings and NoDelay
fixture hardening are pushed; PR/push CI 37564346322/37564341366 both finish
with all eight jobs successful, including Windows installer/release checks.
NoDelay is not a proven explanation of earlier CI failures.
Camera runtime/adapters/binary IPC/all outputs/recovery metadata,
coordination and every original acceptance/final gate remain required.

Native capture admission checkpoint (local): a permit precedes the core capture,
accounting for final pixels, Vec-to-Arc staging, a bounded worker header and encoded
metadata. Core rejects above-ROI replies before allocation and image-size overflow.
Adoption compares the full exposure, moves the pixel Arc and preserves every core
metadata field in bounded immutable JSON. Overflow fails without truncation; the
last reader retains pixels and metadata capacity. This is payload accounting, not
whole-process or decoded-tree RSS accounting. No retries/deadlines are added.
Twelve image cases, three native worker-simulation capture cases and three core
reply cases pass, including retained-frame recovery, pinned-reader admission and
pre-dispatch/during-exposure cancellation. Full core/hub/Alpaca Rust, strict Rust
1.99 Clippy, Rust 1.89 and generated contracts pass. Fresh-host NINA 228/228 and
real net48 x86/x64 regressions also pass. Native source lifecycle/runtime/config,
host budget wiring, recovery
allowances, all camera outputs and every original later gate remain open.

Native owner checkpoint (local): one core Session now retains connection, capture
and cleanup tasks independently of waiters. Atomic admission reserves runtime
activity and payload before publishing markers; invalid/busy starts preserve the
prior image. Snapshots remain readable during capture. Operation/generation guards
reject stale images/errors and cleanup. Explicit Abort waits for core cleanup;
reset fences synchronously and retains cleanup before reconnect. Eight real
worker-simulation cases pass, including last external-reference loss, joined
connections, dropped capture/abort/close waiters, no-dispatch reset, no-retry error
codes and pinned-reader preservation. Full Rust/strict Clippy/MSRV/contracts,
fresh-host NINA 228/228 and real net48 x86/x64 pass. Source factory/runtime/config,
typed native properties/settings, recovery allowances and all outputs remain open.
Review refinement: core cooler queueing is desired state only during capture, and
the direct worker rejects capture-time writes. Add a common acknowledged SDK/direct
cooling path and preserve live targets across recovery; do not equate queued intent
with an applied write. This remains part of the original native camera scope.
Current ef0748e PR/push CI 37564346322/37564341366 both finish with all eight
jobs successful, including Windows packaging/installer checks. Earlier failure
causes remain unproved. Native admission/owner increments can proceed to their
own CI together with the reviewed direct cooling increment; local checks pass.

Direct cooling checkpoint (reviewed; CI required): one bounded request slot is serviced only
by the existing USB owner at its environment checkpoints. Capture-time target and
enable writes wait for an applied/readback acknowledgement; active getters read
acknowledged telemetry. Unsent expiry skips USB; unknown dispatched outcomes fence
additional cooling writes and are typed non-retryable by the core, which retires
the worker. Core also records framed write admission so outer timeout/cancellation
or lost/malformed acknowledgement cannot bypass uncertainty classification.
Private still/video simulations pass with unchanged frame pixels and rejected
unrelated imaging writes. A parked simulated worker verifies outer timeout,
post-dispatch cancellation and process retirement; pre-dispatch cancellation
consumes no command ID. Full final core/hub/Alpaca Rust, all 96 ZWO library tests,
25 core tests, strict Rust 1.99 Clippy and Rust 1.89 all-target checks pass.
Fresh-host NINA 228/228 and real net48 x86/x64 also pass after the outer-deadline
correction. Generated contracts and formatting pass. This
does not complete native cooling: the common Session/NativeCamera acknowledged
queue, recovery-target preservation, source adapter/runtime and every frontend
image output remain required. Camera creation remains disabled.

Core cooling checkpoint (reviewed locally): Session now exposes one
bounded acknowledged target/enable mailbox shared by SDK/direct captures. Queued
and claimed/preflight requests remain cancellable without I/O; dispatched work
survives waiter loss and fences after uncertain acknowledgement. Set plus readback
use one absolute deadline, capped by the configured core command timeout. Expiry
before framed write preserves the worker and consumes no command ID. Successful
acknowledgement updates applied values, shared status and the active recovery map
before publishing its receipt, changing only cooler keys in frozen capture settings.
Recovery reconnect/settle, exposure polling and completed-download checkpoints
service that mailbox on the existing owner. Close/invalidate/Drop retire requests.
Six mailbox cases and actual SDK/direct capture/recovery, mismatch/no-retry, idle
and teardown cases pass, including live target/disable during recovery settle.
All 37 core tests, full core/hub/Alpaca regressions, strict Rust 1.99 Clippy,
Rust 1.89 all-target checks, generated contracts, rebuilt-host NINA 228/228 and
actual net48 x86/x64 clients pass. Logs and review refinements are in hub-review.md.
Preceding pushed native admission/owner/direct-cooling head 6584e67 now passes
all eight jobs in each PR/push run 37568068870/37568064681, including Windows
packaging/installer checks. The reviewed core and NativeCamera increments pass
local validation and require their own CI. All original later gates remain required.

Native cooler owner checkpoint (reviewed locally; new CI required): NativeCamera now
retains one SDK/direct target/enable command and runtime activity independently
of its caller. Caller loss withdraws unsent work; dispatched receipts remain owned.
Capture services the mailbox on its existing engine; idle work acquires that same
engine without blocking receipt completion behind a capture. Generation checks
reject both late completions and buffered acknowledgements after reset. Pending
cooling blocks capture/abort and image publication; an uncertain result retains
its redacted error/code, stops publication and requires explicit reset/close before
new work. Existing readers retain their immutable image. No thermal wait, second
worker, new retry or fabricated StopExposure is introduced.
Fourteen production-worker simulation owner cases, three owner unit cases and
seven core mailbox cases pass. Full core/hub/Alpaca Rust regressions, strict
Rust 1.99 Clippy, Rust 1.89 all-target compatibility, generated contracts,
rebuilt-host NINA 228/228 and actual net48 x86/x64 clients pass. Native
typed properties/settings, source factory/runtime/config, host budget and recovery
allowances, camera adapters/outputs and every original later gate remain open.
Camera creation stays disabled.

Camera supervisor checkpoint (local): nineteen private virtual-clock cases pass.
One source-owned acquisition retains control and runtime activity after caller
cancellation/disconnect, freezes geometry and available exposure identity, and
publishes one immutable shared image. Owner Stop preserves shortened exposures;
Abort discards them. Admission cancellation sends no exposure, cancelled command
preflight sends no Abort, and a dispatched Abort finishes after its waiter leaves.
Explicit notifications release a long-polling monitor without waiting for its
timer. Uncertain replies/deadlines retain ownership; old generations, malformed
readiness/metadata and replaced images cannot publish. Optional metadata remains
independently unsupported. Full Rust hub/Alpaca, explicit Rust 1.99 strict Clippy,
Rust 1.89 all targets, generated contracts, rebuilt-host NINA 228/228 and actual
net48 x86/x64 regressions pass. New-head CI remains required.
Runtime integration, native/Alpaca/COM/virtual/simulation camera adapters, all three
image outputs, recovery metadata and host-wide staging budgets remain open.

Upstream camera ImageBytes checkpoint (local): seven loopback cases pass with the
real Alpaca backend, source actor and acquisition supervisor. The camera HTTP
client shares credentials, connection bounds, redirect denial and no-retry policy,
but the scalar request timeout does not truncate the separately bounded image
download. Streaming preserves full U16 values and transaction/shape checks,
releases partial allocations on cancellation and never repeats a download.
HTTP/JSON/binary error codes are retained with redacted text. Non-camera sources
reject image reads without I/O. Bounded JSON decoding is implemented in the later
checkpoint above; runtime/frontend camera gates remain open. Full Rust hub/Alpaca, strict Rust
1.99 Clippy, Rust 1.89, generated contracts, rebuilt-host NINA 228/228 and actual
net48 x86/x64 regressions pass. New-head CI is required before acceptance.

Current position: milestones 0 and 1 are complete. The scalar source/output paths
in milestones 2 and 3 are implemented; their remaining acceptance gates are open.
Milestone 4 has focuser and rotator publication through all three frontends.
Rotator COM, virtual and dedicated simulation inputs and shared creation are
implemented. Wheel publication through all three outputs is pushed. Windows COM
wheel imports and virtual wheel inputs pass local full regressions and are pushed.
Dedicated wheel simulation passes local full Rust, NINA and both-architecture
net48 confirmation, with a verified native setup capture. Shared wheel creation is
verified locally in both setup frontends and real net48 clients, and pushed.
Panel controller/runtime/IPC/cache and Alpaca V2 publication are pushed at 36a5558,
with full local Rust, NINA 214/214 and both-architecture net48 checks.
Panel Alpaca V2 publication is verified
with dynamic identities, legacy V1/modern V2 inputs and production OFP2 simulation.
Native NINA/ASCOM panel publication is pushed, using shared protocol,
selection/setup and registration paths. Final validation is recorded below.
Panel Windows COM imports are pushed through the existing STA worker;
30 private worker and 19 registered parent cases pass. Virtual panel inputs are
implemented and pushed; six loopback and a production OFP2 simulation case pass.
Full virtual-panel Rust, strict checks, NINA 223/223 and real net48 x86/x64 pass.
Dedicated panel simulation has 35 passing Rust simulator cases, three new NINA
cases and real net48 x86/x64 coverage. Full regressions and strict checks pass;
shared panel setup passes full Rust/strict checks, NINA 228/228 and real net48
x86/x64 checks. Actual native/browser captures and browser creation/reload/sparse
update acceptance are verified. Camera proxies remain. The first camera increment
adds validated immutable image buffers, a shared payload budget and a lossless
ImageBytes reader/export codec, with native adoption and bounded order conversion.
The shared source actor now has fenced binary download dispatch with exclusive
control, a separate bounded image deadline and retained write uncertainty.
Acquisition supervision and shared properties/settings now pass thirty focused
tests; binary/JSON Alpaca image inputs are implemented. The retained native owner
and acknowledged SDK/direct cooler mailbox now pass local validation, preserving
live targets across recovery and fencing publication after uncertain commands.
Native RAW16 property mapping, desired bin/ROI selection and acknowledged native
gain/offset commands pass full local regressions and review; their CI remains
required.
Runtime integration,
capability/recovery propagation, remaining camera adapters and
frontend image transport remain unimplemented. Their contracts are recorded in
hub-contract.md and camera choices stay gated.
Milestone 5's coordinated groups are not yet
implemented. PR #21 stays draft until the full plan passes.

Windows panel import checkpoint: both helper architectures use the common strict
member tables, typed validation, V1/V2 connection policies and source ownership.
Private tests cover actual state enums, independent completion and components,
live brightness limits, On(0), shared leases and applied lost replies that fence
every sibling command without automatic actuator cleanup. Thirty worker and 19
registered parent cases pass. Review corrected a fixture-only assumption that
poll recovery cannot open a new worker; no production recovery policy changed.
Full local Rust hub/Alpaca, strict Clippy, Rust 1.89, contract freshness, Node/six
schema checks, rebuilt-host NINA 223/223 and real net48 x86/x64 clients pass.
Panel COM choices stay gated until virtual inputs, simulation and shared creation
are verified. Next: virtual panels, then dedicated simulation/shared creation and
all remaining camera, coordination and original acceptance/final gates.

Virtual panel checkpoint: the existing bounded typed composition path now supports
six properties and five commands. Cached polling preserves dependency ages and
per-property errors without extra leaf I/O. Six two-layer loopback cases cover
V1/V2, slow/cancelled connection, unknown completion, lost generations/replies,
On(0), shared ownership and partial recovery. Production OFP2 explicit simulation
also passes through two layers. All 25 panel and 23 native cases, full Rust
hub/Alpaca, strict Clippy, Rust 1.89, contract freshness, Node/six schema checks,
rebuilt-host NINA 223/223 and real net48 x86/x64 clients pass. Review evidence
records corrected fixture assumptions about typed versus scalar validation and
idle inner leases. The host rebuild initially encountered a test-owned Windows
executable lock; after the running Rust suite finished, sequential rebuild and
frontend confirmation passed. No equipment or installed vendor driver was used.
Next: dedicated panel simulation and shared creation, then all original remaining
camera, coordination, recovery, documentation and final acceptance gates.

Dedicated panel simulation checkpoint (local): a new V2 simulated source reuses
the shared actor/controller, polling, virtual composition and all three outputs.
Independent cover/light clocks, atomic sparse updates, Int32 brightness bounds,
On(0), absence and completion semantics, stalls/stopped-short/malformed replies
and retained write uncertainty are implemented. Native/web editors share ten
generated controls and reject inconsistent compound status. Thirty-five Rust
simulator cases, the new actual HTTP case, Node/seven schema checks, NINA 226/226
and real net48 x86/x64 clients pass. The actual WPF capture is visually verified
and included in the setup guide. Full Rust hub/Alpaca, strict Clippy, Rust 1.89
all targets and generated-contract freshness pass. Sequential confirmation
followed an overlapping NINA host causing a Windows executable lock; no running
test was interrupted. Shared panel creation stays gated until its
own acceptance. No hardware or installed vendor driver was used.

Shared panel creation checkpoint (local): the existing generated native/browser
forms enable CoverCalibrator proxy, COM and simulation choices. Two real net48
clients and NINA verify stable save/reload identities, independent leases,
shared On(0) and actual cover completion. Actual WPF/browser captures are visually
verified. Browser acceptance creates outputs 7/8, rejects a class mismatch,
retains both UUIDs on reload and applies a cover-only simulation edit while light
state and configuration revision remain unchanged and leases return to zero.
Full Rust hub/Alpaca, strict Clippy, Rust 1.89 all targets, generated contracts,
Node/seven schema checks, NINA 228/228 and real net48 x86/x64 pass. Original
fixture-choice/namespace failures are retained and explained in the review log.
Preceding simulator/Automation-fix CI 37554304962/37554298269 is live; keep this
increment local until it finishes. Camera buffer/codec foundation now passes
eleven private tests plus interoperability with the existing Alpaca camera
response, full Rust hub/Alpaca suites, strict Clippy, Rust 1.89 all targets and
contract freshness. Acquisition ownership/capability and binary transport
requirements are recorded in hub-contract.md. Next: implement source-owned
acquisition supervision and camera inputs/outputs, then every remaining original
gate. Camera proxy creation stays disabled.

Camera binary source checkpoint (local): eight private actor cases pass alongside
the full Rust hub/Alpaca suites, strict Clippy, Rust 1.89 all targets and generated
contract freshness. A freshly rebuilt host passes NINA 228/228 and actual net48
x86/x64 regressions. Image dispatch requires source control and a matching
generation, uses its own bounded download deadline and preserves uncertainty.
The acquisition supervisor must retain runtime activity after a frontend
disconnect; camera adapters and all three image publications remain open.
Preceding panel CI 37554304962/37554298269 now finishes successfully in both runs,
all eight jobs including Windows installer acceptance. Reviewed shared panel
creation, camera image buffers/codec and binary source dispatch proceed to the
same draft PR. New-head CI is required; earlier unrelated intermittent failures
remain documented rather than being declared resolved by these green runs.

Filter-wheel controller increment: shared typed sessions, live ordered names and
signed offsets, slot bounds, nonblocking Position writes and moving `-1` are
implemented locally. Names/offsets must have matching bounded lengths and a zero
reference offset. Shared cache validation now admits bounded flat metadata arrays
while retaining aggregate text/item limits across partial updates. Private actors
and actual loopback Alpaca V2/V3 cases cover ownership, motion and uncertainty.
Native EFW adapters now expose ordered metadata through the existing worker.
Optional saved `filterWheel` arrays retain names and signed offsets across
calibration/reconnect; absent metadata uses numbered names and zero offsets.
Explicit metadata must match hardware slots, including on the low-level write
path. Wheel runtime/IPC and cached diagnostics are implemented and verified
locally. Alpaca publication is verified locally; native NINA/ASCOM publication
passes managed checks. Windows COM imports pass private worker/parent checks;
shared creation is verified locally in setup; broader acceptance remains open.

Shared wheel polling increment is locally verified: the existing property poller supports
bounded string and Int32 arrays, and factory plans deduplicate wheel metadata and
Position across outputs. One SampleBudget now enforces flat-array/text admission
in both the source cache and collected Alpaca results. Twenty-seven actual Alpaca
transport cases, seven factory cases, eleven wheel-controller and twenty-six
source-actor cases pass, along with strict Clippy. Full Rust hub/Alpaca suites,
Rust 1.89, contract freshness, rebuilt-host NINA 202/202 and real net48 x86/x64
regressions also pass. The current local runtime increment adds shared wheel
sessions, typed IPC and cached diagnostics. Its fifteen wheel, eighteen focuser
and twenty-two rotator tests pass, including oversized replies, cancellation,
uncertain moves and independent leases. A new production EFW worker test in
explicit simulation verifies runtime metadata/polling/reconnection; browser
reader and five independent schema checks pass. Full Rust hub/Alpaca suites,
strict Clippy, Rust 1.89, generated-contract freshness, rebuilt-host NINA 203/203
and real net48 x86/x64 clients pass. Cache/IPC confirmation additionally verifies
per-key error recovery, omitted unavailable Position and oversized diagnostic
replies without stream loss. Native wheel publication verification and shared
creation remain required.

Wheel Alpaca increment: the existing typed HTTP adapter publishes V3 with shared
connection/DeviceState negotiation and source-owned Names, FocusOffsets and
Position. Dynamic identities/numbers, local wheel 0 coexistence/collision,
strict values/metadata and uncertain-write fencing are covered by five new
actual HTTP cases, including the production EFW worker in explicit simulation.
All 31 router cases, full Rust hub/Alpaca suites, Clippy, Rust 1.89, generated
contracts, Node/five schema checks, rebuilt-host NINA 203/203 and real net48
x86/x64 clients pass. Common setup-page routing is shared with focusers and
rotators. Keep this increment local while preceding CI finishes; native wheel
publication verification and every original later gate remain required.

Native wheel output increment: NINA and ASCOM share request/value validation,
saved selections and existing setup/registration. NINA preserves profile filter
settings, rejects profile changes during connection and exposes nonblocking
Position. ASCOM exports V3/V2 and signed Short Position in DeviceState. Both
retain actual source state, independent leases and uncertain-write fencing.
All 209 NINA checks, real net48 x86/x64 clients, both-architecture warning-denied
staging, seven-output manual COM exports, Node and five schema checks pass.
Cold HKCU SCM activation fails at the first existing Switch class, as previously
observed; wheel cold/production registration acceptance still needs CI.
Full Rust hub/Alpaca regressions, strict Clippy, Rust 1.89, generated contracts,
fresh-host NINA 209/209 and real net48 x86/x64 confirmation pass.

Windows COM wheel increment: existing STA workers now expose three typed reads
and Short Position writes with shared V2/V3 connection policies. SAFEARRAY
rank/count/types and strict UTF-8 bounds are checked before serialization;
parent admission reuses the Rust wheel decoder only for metadata reads. Actual
private tests pass 27 worker and 16 registered parent cases, including shared
array polling, sibling leases, malformed metadata and uncertain setters without
replay. Review corrected a fixture HRESULT and the real scalar-only parent array
gate exposed by those tests. Full Rust hub/Alpaca, strict Clippy, Rust 1.89,
contracts, Node/five schema checks, fresh-host NINA 209/209 and real net48 x86/x64
pass. Generated wheel COM choices stay gated until shared wheel creation.
Virtual wheel increment: typed connections and cached array forwarding now reuse
the focuser/rotator paths. Six two-layer loopback cases cover V2/V3 ownership,
slow/cancelled connection, lost generations/replies, actual motion, metadata
dependencies, per-key recovery and preserved ages without extra leaf I/O. All
21 wheel and 20 native tests pass, including a new two-layer production EFW
worker test in explicit simulation. The first cache test expected Unavailable
instead of the Alpaca sampler's Permanent error; the corrected assertion checks
that every layer preserves the original error and valid metadata. Full Rust
hub/Alpaca regressions, strict Clippy, Rust 1.89 all targets, generated contracts,
Node/five schema checks, rebuilt-host NINA 209/209 and real net48 x86/x64 pass.
Reviewed COM/virtual increments are now pushed at a586c76 after preceding
publication PR/push CI passes all eight jobs.

Dedicated wheel simulation increment: the existing shared actor/timed-motion
path now supports typed wheel metadata and Position, atomic sparse state updates,
stalls/stopped-short/invalid motion and retained write uncertainty. Both setup
frontends consume generated bounded string/Int32 array controls. Actual private
IPC proves an update can apply before its reply exceeds the frame budget; both
frontends now require reload rather than permit another mutation after that
response. All 27 simulator cases, the new HTTP wheel case, full Rust hub/Alpaca
suites, strict Clippy, Rust 1.89, generated contracts, Node/six schema checks,
rebuilt-host NINA 212/212 and real net48 x86/x64 clients pass. The actual native
capture is visually verified. Review corrected optional-array schema admission,
boxed actor updates and local JSON error classification. This increment is pushed
after preceding a586c76 CI passed all eight jobs in both runs. It is committed at
04ae046. Shared wheel creation now uses the existing schema-driven editors:
FilterWheel has its own output capability, with COM/simulation input choices
enabled. Full Rust hub/Alpaca, strict Clippy, Rust 1.89, generated contracts,
Node/six schema checks, NINA 213/213 and real net48 x86/x64 pass. Native WPF and
browser captures are visually verified. Browser acceptance creates outputs 7/8,
rejects a class mismatch, preserves saved IDs on reload and changes only Names
with zero source leases; invalid local JSON leaves the editor usable.
Next: panels, then all
remaining typed devices/cameras/coordination and original acceptance/final gates.

Runtime checkpoint push CI evidence: Windows initial focuser Connect receives
uncertain before the lost-Move test dispatches motion. Its loopback PUT connected
trace closes without a reply; cause is unproved. The fixture now records caught
transport exceptions in its existing failure-only trace. Intel macOS asserts
upstream disconnection immediately after client Connecting=false, although
SourceLease::drop schedules cleanup asynchronously. The test now awaits that
observable cleanup within its existing three-second budget, retaining both the
disconnect assertion and independent other-source connection assertion. The
full local Rust suites pass; portable CI must verify the correction.

Retained reliability issue: simulator checkpoint d4050c1 PR run 37530345334 completes
with seven successful jobs and a Windows failure. The focuser lost-Move-reply
fixture expects `uncertain` but receives `transient`; that assertion alone does
not prove the command was dispatched. Added failure-only request/dispatch and
source-state evidence to both accessory fixtures without changing assertions,
retries or deadlines. The focused cases and all 201 NINA tests pass locally.
Root cause and new CI confirmation remain required; push run 37530337700 is
now complete with all eight jobs successful. That pass does not explain the PR
failure. Shared creation and diagnostic commits proceed with the locally checked
wheel controller into the same draft PR.

New exact-head CI evidence at bf15ced: PR run 37533746765 completes seven jobs
successfully but Windows fails the real net48 x86 rotator fixture at a Sync
operation with `uncertain`. Its 201 NINA tests pass. The existing short failure
message does not identify loopback versus simulated Sync or the dispatch/reply
history; the cause remains unproved. Failure-only checkpoint, source-health,
request-timing and full-stack diagnostics are added locally without changing
production behavior, retries, assertions or deadlines. Push run 37533737225
subsequently completes all eight jobs successfully, including Windows packaging,
installer and camera kit acceptance. It does not explain the separate PR failure.
Native metadata head 630b302 PR/push runs 37536023962/37536015902 subsequently
complete all eight jobs successfully. Their passes do not establish earlier causes.

Completed increment: shared setup enables rotator proxy creation and installed COM
rotator choices through the existing generated forms. Native NINA and real net48
fixtures create two outputs for one simulated source, preserve saved IDs and
verify independent leases and shared coordinates. Actual WPF and in-app browser
acceptance cover creation, review/apply/reload, source-class errors and inert setup.
Browser simulation acceptance verifies sparse updates and Single boundaries.
Full local validation is recorded in the progress log. This closes implementation
of rotator creation; broader hardware/conformance acceptance remains open.

Completed increment: dedicated rotator simulation uses the existing typed source,
controller and outputs with shared timed motion, source-owned coordinates,
optional capabilities and fault injection. Native/web editors use twelve generated
controls, including common Single bounds and nested state validation. Twenty-one
simulation cases, full Rust hub/Alpaca suites, Clippy, Rust 1.89, contract freshness,
Node and four schema checks pass. A rebuilt production host passes all 199 NINA
tests and real net48 clients in both Windows architectures. The setup guide includes
an actual WPF simulation screenshot. This checkpoint still needs its own CI;
shared rotator creation and every original remaining gate stay open.

Completed increment: Windows COM rotator imports use the same isolated STA workers,
typed controller, polling and connection leases. V2/V3 negotiate legacy Connected;
V4 uses asynchronous ownership. Strict Single boundaries reject invalid readings
and commands before dispatch, including rounding to 360 and step-size underflow.
Source-owned coordinates and signed relative distance remain separate. Shared
uncertainty cannot replay commands or invoke an automatic Halt. Canonical typed
ASCOM identities now reject self-proxies during config validation too. Twenty-four
private worker and fourteen actual registered parent tests pass on both Windows
architectures, alongside final Rust hub/Alpaca, Clippy, Rust 1.89 and contract checks.
Rebuilt-host managed confirmation passes all 196 NINA tests and real net48 clients
on both architectures. Exact-head CI is recorded in the progress log. Dedicated
rotator simulation/shared creation and every original broader gate remain open.

Completed increment: shared simulation controls in native NINA/ASCOM and web setup.
The host describes field paths, labels, defaults, physical limits, fault choices
and deadlines. Editors change only selected fields on saved, explicitly simulated
sources, check the saved revision and reject uncertain replies without replay.
Read current state resets the form without opening equipment. Weather sensors can
be marked absent. Safety still uses normal polling and confirmation; clearing an
injected fault never clears a retained uncertain-write latch. Actual WPF/browser
screenshots are documented as simulation. Numeric Switch wire keys now decode
through internally tagged commands and reject duplicate/noncanonical keys.

Completed increment: first-time creation in the shared native NINA/ASCOM selector.
It uses the same Rust no-clobber path as CLI setup, retains a selected filename
before dispatch and blocks another creation after unknown completion. Explicit
bounded file read reconciles identity/absence without starting a host. The filename
stays copyable while protected from editing. Loading an empty file enables the
common editor even with no selectable outputs. Local tests prove those flows;
broader diagnostics and interactive setup acceptance remain required.

Completed increment: shared cached output diagnostics in the Rust host/IPC and
protected HTTP setup API. Saved-revision checks and pages of at most 32 items
cover safety memberships, reserved Switch slots and weather metrics. Safety
reports the existing controller's raw/effective decision, reason, counters,
age/hold and policy; an inactive controller remains unknown/unsafe even when
another owner has cached safe data. Reads cannot accelerate recovery or clear
uncertain writes. Switch uses its operational freshness/bounds interpretation;
weather uses a private, projected copy of the same averaging/fallback engine.
Sample and policy generation/revision identities remain visible. Local Rust,
HTTP, native-client and real net48 checks pass. Native and web setup now present
these observations and export them with time/revision, using the generated wire
schema plus saved identity/page checks. Cache reads preserve review; lost,
malformed or obsolete replies require explicit Reload. Actual WPF/browser
screenshots document simulation, with no equipment leases.

Completed increment: real actor polling diagnostics in cached source/output health,
the generated wire schema, native/web summaries and observed exports. Publications
include connecting/sampling/waiting/suspended phases, started attempts, cycle
completion and backoff counters. The remaining wait belongs to its monotonic
observation time; it is not a live countdown or a command replay promise. Paused-
time fixtures prove actual dispatch, Retry-After/exhaustion, partial passes and
stalled I/O. Browser verification uses an explicitly simulated source and preserves
the one independent output lease; cleanup releases it. Local Rust, 171 NINA tests,
Node/schema and real net48 clients pass.

Native-credential checkpoint 2a26281 passes all eight jobs in both PR CI
37457955380 and push CI 37457946553. Windows logs prove installed metadata without
host/equipment activation, nested helper busy guards and natural retirement,
upgrade preservation, failed uninstall with conflict, orphan removal after
deleting bindings, preservation of another installation/settings and cleanup.
Web-credential checkpoint a2cad0a passes both complete CI runs 37460531169 and
37460523532. Native-inspection checkpoint 94b9adc passes PR CI 37462088304 and
push CI 37462081747, including all eight jobs. Simulation checkpoint 6f36ff5
passes seven jobs in both runs, but Windows fails separate pipe fixtures: a
partial-frame sender race and cold handshake scheduling. This increment removes
the test scheduling races without raising production/test deadlines. CLI
checkpoint eeb9208 passes both complete runs 37466565218 and 37466555917.
Native-creation 3dd86b4 PR run 37468454801 passes all eight jobs.
Push run 37468447863 fails the first x86
COM import fixture on its five-second response wait; all NINA and net48 fixtures
passed before that failure. Preserve and investigate that timeout; its cause is
not yet proved. Diagnostic API de2691a PR 37471446346 and push 37471435465
pass seven jobs, including Windows; Intel macOS exceeds the outer 15-minute job
budget. Logs show passing Rust suites and cold build time, not a hung test. The
portable CI job budget is now 25 minutes; test/transport deadlines are unchanged.
Frontend checkpoint 96ea4e3 PR 37475434423 and push 37475427160 both pass all
eight jobs, including Windows and Intel macOS. Polling checkpoint 400a74d PR
37478747826 fails Windows in the native safety-expiry fixture: its expected
unchanged source generation changed. The reset's cause is not proved by that
assertion. The fixture now supplies and verifies an acknowledged Retry-After
longer than the safe-data lifetime before checking expiry, retaining unchanged-
generation, stale-state, independent weather and recovery assertions. Production
deadlines/policies are unchanged. New CI must verify this correction; push
37478744401 and empty-profile runs 37479613856/37479602454 remain live.
Interactive UAC/Chooser, conformance and
vendor acceptance remain required.

Completed refinement: accessory-only HTTP publication accepts an explicitly empty
camera-profile list. Missing settings retain the ordinary main/guide defaults.
The root setup shows an empty state and can add/save the first slot. Reload uses
the same profile/unique-ID checks as startup before replacing in-memory settings.
Real executable startup/restart, catalog, invalid-camera requests, profile
persistence, first-slot setup and actual browser acceptance pass without equipment
activation. This does not close broader setup, discovery or frontend acceptance.

Completed increment: Alpaca focuser publication through the shared typed IPC.
Configured UUIDs and sparse numbers survive discovery and routing, including
multiple independent sources and outputs sharing one source. The V4 interface
exposes typed properties, bounded absolute/signed-relative moves, optional errors,
cached DeviceState and asynchronous connections. Lost move replies retain the
shared uncertainty latch without command replay or silent generation adoption.
Local focuser slots coexist at distinct numbers; collisions fail explicitly before
equipment connection. Per-output setup opens the shared editor. Private loopback
tests exercise the production adapter, host, IPC and HTTP router; no physical
equipment is actuated. Conformance and broader frontend acceptance remain open.

Completed increment: native NINA and ASCOM focuser outputs use shared typed request
builders/value validation, immutable saved choices, the existing themed selector
and stable Focuser Chooser registration. NINA waits for absolute motion completion
and verifies the target; relative sources stay available through ASCOM/Alpaca.
ASCOM exposes V4/legacy interfaces and integer Position in DeviceState. Both retain
independent leases, source-generation fences and shared uncertainty; cancellation
does not replay a command or issue an automatic Halt. Private NINA and real net48
x86/x64 adapter/COM export fixtures pass. Disposable Windows CI now proves cold
SCM activation for all five output types on both architectures. Both frontend CI
runs fail the production-registration fixture's missing Focuser Chooser mapping;
the mapping is corrected and needs new CI. Local HKCU SCM activation failed on
the first existing Switch class. Proxy setup was gated at this checkpoint; current
focuser setup is recorded below.

Completed increment: Windows COM focuser imports reuse isolated STA workers,
typed source polling and shared controller leases. V3 uses legacy Connected and
V4 uses asynchronous connection in both parent and worker. Strict Int32 limits,
typed properties/commands and retained uncertainty preserve the same semantics
as native/Alpaca sources. Twenty worker tests and twelve actual registered parent
tests pass, using private fixtures only. No installed vendor driver is activated.

Completed increment: virtual focuser inputs use the shared typed controller and
retain private inner clients. Incremental supervised connection separates the
inner connection deadline from an outer request step. Cached polling preserves
typed values, per-property errors and original sample ages; invalid inner sessions
retire the virtual transport. Seven private loopback cases cover nested motion,
relative moves, limits, optional errors, age, pending connection/cancellation and
uncertainty without replay or automatic Halt.
An eighth production-worker test proves explicit EAF simulation remains labelled
through both virtual layers and completes motion without a hardware fallback.

Completed increment: dedicated typed focuser simulation shares the existing actor,
controller and source leases, with no SDK or hardware fallback. Rust describes
fifteen native/web controls, including strict Int32 coordinates/limits, optional
properties, monotonic motion duration and class-specific faults. Atomic sparse
updates cannot partially alter state. Disconnect does not Halt; relative motion
does not invent Position. Invalid motion reads, stalls, stopped-short completion
and dispatched uncertain moves remain explicit. Clearing a fault cannot clear
the shared uncertainty latch or reconnect an obsolete session. Fifteen Rust
simulation tests, all 184 NINA tests, real net48 x86/x64 clients, shared Node/schema
checks, strict Clippy and Rust 1.89 checks pass. An actual WPF screenshot is labelled
simulation; browser rendering is accepted in the next setup increment below.
COM checkpoint 8c806d5 push CI 37494616586 passes all eight jobs; its PR run
37494625707 was cancelled after seven successes. This verifies the corrected
Focuser Chooser mapping in disposable Windows CI. Virtual checkpoint 1af147b
PR/push runs 37496570309/37496563543 remain live at this observation.

Completed increment: shared native/web setup can create Focuser proxies. The host
advertises proxy creation with class-specific capability rules: only Focuser is
enabled; unfinished classes remain unavailable. Windows COM setup now exposes its
implemented Focuser class while respecting installed worker bitness. Actual native
creation preserves IDs, validates relationships without leases and connects two
outputs to one simulator after Apply. Chrome acceptance proves source/output
creation, reviewed save/reload, rejection of fractional coordinates, sparse integer
updates and zero source leases with no console errors. It exposed a tagged-choice
handler capturing a mutated schema; the handler now retains its original choices
and has an actual form-event regression. All 186 NINA tests and real net48 x86/x64
clients pass. Virtual checkpoint push CI 37496563543 passes all eight jobs; its PR
run 37496570309 was cancelled. Simulation checkpoint CI is still running.

Completed increment: typed rotator controller with shared accessory session
ownership. Focuser and rotator controllers now reuse connection readiness,
immutable generation fences and unique command leases without sharing device
semantics. Rotator reads preserve logical, mechanical and target angles separately;
relative commands retain their signed distance. Live motion and reversal checks,
optional errors, cancellation and dispatched uncertainty remain explicit. Eleven
private actor/actual Alpaca V3/V4 transport tests pass, including an applied move
with a malformed acknowledgment and no replay. Full local hub/Alpaca regressions,
Clippy, Rust 1.89, generated-contract, Node/schema, all 186 NINA tests and real
net48 x86/x64 clients pass. This controller is not yet enabled in runtime/IPC,
simulation, COM/virtual imports or any output frontend. Native source adapters
and reference persistence are implemented in the following increment; runtime
and frontend publication remain gated.

Implemented increment: native CAA/Falcon typed properties and persistent reference
handling. Sync/Reverse save an uncertainty marker before dispatch, verify the
reported result and atomically commit the confirmed offset/direction. A lost reply
or ignored Sync survives complete source recreation as an unknown logical
reference; only explicit reconciliation can make logical movement available.
Reconnect restores a matching saved transform through a local worker operation
without changing hardware direction, origin or position. A direction mismatch
leaves logical coordinates unavailable and requires explicit Sync. Records are
scoped to the user/configuration, source, vendor, identity and simulation mode;
malformed records fail closed rather than becoming zero offsets. Native relative
movement retains its existing single-command +/-360 degree limit.

Simulation checkpoint PR/push CI 37499571887/37499559138 passes all eight jobs.
Shared-setup push CI 37501487430 fails Windows during an initial NINA focuser
connection, before the injected uncertain Move. Its cause is unproved. The fixture
now reports the structured remote error, source snapshot and private request
timings without retries or deadline changes. Preserve this failure alongside the
earlier COM timeout; passing local tests alone do not close either investigation.
Shared-setup PR CI 37501496852 passes all eight jobs. Rotator-controller PR CI
37503876752 also passes all eight jobs; push 37503869679 was cancelled after seven
successes. These successes do not explain the retained initial-connection failure.
Native-reference PR CI 37508673278 and push 37508667983 now pass all eight jobs.
These successes do not establish the causes of the retained fixture failures.

Implemented increment: native/Alpaca rotator outputs in the shared source runtime
and private typed IPC. Sparse saved identities, independent connection/command
leases, pending cancellation and immutable generation fences reuse the existing
host path. Polling deduplicates seven typed properties per source. Paged diagnostics
and DeviceState read cache only and preserve per-property errors and sample ages;
native/web readers share the generated schema and property/range descriptions.
DeviceState contains only the three standard rotator operational properties;
Reverse and TargetPosition remain in typed reads and diagnostics. Review also
fixed the combined poll limit to include typed properties after deduplication.
Native NINA/ASCOM rotator publication, imports, dedicated simulation and shared
creation are still gated; the configuration editor enables only Focuser proxies.

Implemented increment: Alpaca rotator publication through the shared typed IPC.
Saved UUIDs and sparse numbers route independent sources and clients sharing one
source. Seven typed reads and six commands preserve signed relative distance,
separate logical/mechanical coordinates, optional StepSize errors and uncertain
outcomes without replay. Cached DeviceState omits failed properties while retaining
valid mechanical readings. Local rotator slots coexist at distinct numbers;
collisions reject catalog, reads, writes and setup before opening equipment.
Per-output setup uses the existing shared editor; it does not enable creation of
rotator proxies yet. Modern output admission requires CanReverse=true and a strict
Reverse reading within the whole connection deadline. Sources unable to provide
reversal fail synchronous/asynchronous connection with a fixed Regain explanation;
the generic controller still permits legacy capability inspection. Seven private
HTTP cases include actual CAA/Falcon workers in explicit simulation, shared leases,
motion completion and saved reference restoration after worker recreation.

Next: publish rotators through native NINA/ASCOM, then complete
COM/virtual/simulation imports and shared
setup. Continue the remaining typed accessory proxies.
Retain the earlier COM fixture timeout
investigation. Complete camera ownership, coordination and every original
remaining milestone and acceptance gate. Proxy setup enables only implemented classes; broader typed configuration and
publication remain required.

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
- [x] Permit an accessory-only HTTP server with zero camera profiles; retain normal
  camera defaults for new ordinary installations. Verify startup, catalog, setup,
  invalid camera requests and restart without dummy camera slots.
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
The adapter now advertises Switch 3, SafetyMonitor 3 and ObservingConditions 2
when the host negotiates scalar state and modern connection capabilities. It
supports nonblocking Connect/Disconnect, retained Connecting errors, and cached
DeviceState with unavailable readings omitted. The shared host supervises admitted
connection changes and includes them in apply quiescence. Legacy Connected remains
synchronous. Switch channels report CanAsync=false, reject asynchronous setters,
and implement CancelAsync as a validated no-op. Conformance remains unverified.

An initial `/setup/hub` editor now consumes shared Rust schema metadata for fields,
choices, references, units, bounds, defaults, identities, and capability gating.
It keeps a draft and reviewed candidate, validates through the host, applies with
revision checks, and exposes source status/explicit paged inspection. JavaScript
and .NET readers share the additional reference/enum metadata. Tests cover durable
API updates, connection/stale-revision guards, and rejection of cross-origin or
device-command requests. Chrome verified editing, review, and successful apply;
the simulation screenshot is checked in. Credential controls, initialization,
richer diagnostics, and reconnect/resume UI are still needed before setup acceptance.

Local validation for the modern scalar increment passes 181 Windows hub tests
plus the endpoint process fixture and 29 Alpaca tests. Faults cover stale cached
bundles, failed asynchronous connections, overlapping changes, EOF before task
start, retained errors and explicit reconciliation. Clippy, Rust 1.89.0 and fresh
package verification pass. These checks do not establish conformance certification.

The shared .NET client now builds in the existing frontend assembly for .NET 8
and net48. It uses the Rust attachment helper and protected local IPC, with bounded
framing/admission, strict identity/reply validation, cancellation accounting and
no replay. Tests cover a real shared host, separate leases, permissive-pipe denial,
queued/dispatched cancellation and abandoned-client cleanup. Actual net48 x86/x64
processes pass the same public attachment/connect/disconnect API. All 73 NINA
regression/contract tests passed at that checkpoint, including 32 hub-client checks.

Native NINA Switch, SafetyMonitor and ObservingConditions providers now use the
shared private host directly. The chooser reads saved identities without discovery;
one common native selection window saves instance/output UUIDs with revision-checked
atomic updates. It starts no HTTP publisher and acquires no equipment lease.
The host owns source settings, safety decisions, measurement ages and write permissions.
Retired Switch objects are fenced across reconnect; failed writable readback throws
instead of allowing NINA's NaN comparison to report success. Missing capabilities
produce read-only channels with a reconnect diagnostic.

All 88 NINA regression/contract tests pass, including 15 new native checks. These
exercise the production host, actual HTTP publisher sharing/disconnect, tombstones
after durable apply, cancelled connections, uncertainty, per-metric failures and
safe-evidence expiry through HTTP backoff without a source generation reset. Both
native library targets build with warnings denied, and net48 x86/x64 attachment
fixtures still pass. These interface tests are not interactive NINA acceptance.
The native selector now opens a shared-descriptor WPF editor for source mappings,
safety policies and weather settings. A private setup session performs redacted
review, one revision-checked Apply and explicit reconciliation without equipment
leases or an HTTP listener. Saved IDs/numbers remain fixed, new records receive
new IDs, invalid text survives collapsed sections, and collections load lazily
in pages. Cached host/source health is available separately. The shared native
credential tab now supports write-only creation, retained-reference reconciliation,
status and unused-reference removal; web setup supports the same operations.
Explicit native inspection now uses the same host descriptors as web setup,
including bounded Switch pagination. Saved source choices stay independent of
unsaved draft additions. The session checks source/revision/generation and page
cursors, revokes review before a probe, and requires reload after transport loss,
cancellation or obsolete replies. Export includes public host status and the last
source observation with its time/revision; no configuration or credentials are
collected. All 144 NINA tests and real net48 x86/x64 inspection/export fixtures
pass; the actual shared WPF render is documented as simulation. Shared simulation
controls now use the same host descriptors in the native and web editors, with
sparse revision-checked changes, current-state reset, sensor absence and bounded
fault injection. All 149 NINA checks, actual net48 x86/x64 clients and browser
verification pass. Initialization and broader diagnostics remain required setup
refinements.

The common native selector now includes saved-choice management, showing the
configuration path, instance and output identity. Revision-checked removal shares
the existing atomic save/lock path, preserves unrelated entries and never changes
host configuration or connected leases. Unreadable files, stale revisions and
missing identities fail without a write. An actual WPF workflow covers competing
saves and explicit reload; its verified screenshot is labeled simulation. All
114 NINA checks, net48 x86/x64 removal/lease fixtures and warning-denied shared
builds pass locally. This completes basic native selection removal, not ASCOM
registration management or interactive/frontend acceptance.

This editor checkpoint adds 21 checks: draft/schema/control behavior, production
host review/apply, competing revisions, connected-client rejection, lost committed
reply, disposal/cancellation, health transport loss, malformed validation replies and an actual WPF
window workflow. Screenshots are labeled automated simulation. Real net48 x86/x64
processes also exercise editor review/apply/reconciliation. PR CI for native
checkpoint 8e73b27 passed all eight jobs; push CI exposed the stalled-writer
ordering fixture's short timeout, corrected without changing production deadlines
or the separate timeout tests. This increment requires its own CI results.

Next: native setup refinements and interactive NINA acceptance,
COM/vendor acceptance, connection/state/error conformance,
and the remaining shared setup refinements. Basic frontend error translation is tested; complete
protocol conformance remains unverified. Generic
scalar polling does not establish camera image or acquisition support. No complete
milestone 2 or frontend/hardware gate is closed by these library controllers.

### 3. Windows imports and native NINA — first useful release

- [x] Implement isolated COM import with timeouts, explicit connection ownership,
  cached telemetry, and recovery from a hung driver host.
- [x] Add native NINA Switch, SafetyMonitor, and ObservingConditions providers and
  configuration UI using shared descriptors and local IPC.
- [x] Exercise native, Alpaca, and COM inputs through the same policies.
- [ ] Verify NINA operation with no Alpaca listener and, for native/network-only
  sources, no dependency on ASCOM Platform.
- [ ] Verify source sharing between NINA and Alpaca clients, including disconnect
  order, restart, and competing command attempts.

Gate: NINA sees combined channels and dependable safety/weather status; a stalled
COM driver cannot prevent cached safety evidence from expiring. Document exact
tested NINA/ASCOM versions and supported imports.

COM worker checkpoint: `Regain.Hub.ASCOM` now builds x86 and x64 private import
workers for Switch, SafetyMonitor and ObservingConditions. Driver activation,
all calls and RCW release run on one message-pumping STA. The bounded versioned
newline protocol uses the existing accessory transport's outer response shape;
activation waits for the parent's first request. Connection steps distinguish
legacy borrowed connections and modern owned client connections; uncertain
mutations and connection changes cannot replay. Driver HRESULTs survive without
raw exception messages. Ordinary imports cannot invoke SetupDialog, arbitrary
Action/Command methods, connection setters, or Dispose.

`scripts/test-hub-com.ps1` builds both architectures with warnings denied and
runs 16 real HKCU COM-fixture cases in each architecture, including STA/pump,
legacy/modern ownership, typed safety, weather ages and partial errors, switch
writes, malformed frames, failed verification cleanup, uncertain writes, missing
registration and hung-worker isolation. It uses private fail-if-present fixture
registration and removes those keys afterward; no installed equipment driver is
activated. The Rust factory now adopts these workers through the existing bounded
accessory transport. Parent-owned cancellation/deadlines, generation fencing,
retained mutation/connection uncertainty, independent safety expiry and mixed
native-simulation/Alpaca/COM outputs have ten real parent integration cases in
both architectures. Alpaca and COM share the scalar sampling state; installed
worker architecture and supported class choices use shared capability metadata.
Both Windows payloads include private helpers, dependencies and licenses; the
release workflow signs/verifies both EXEs. Local unsigned packaging passes;
updated release signing and interactive frontend/vendor acceptance remain open.

The worker foundation's push CI 37434827510 and PR CI 37434832440 failed Windows
first activation with HRESULT 0x80070002; their seven other jobs passed. Do not
call that checkpoint green. Fixture-only loader diagnostics are added, with no
production activation bypass or raw vendor error text. Adapter 9dfdb82 also failed Windows
activation in push 37438671971 and PR 37438682468. Its fixture probes establish
successful direct managed loads and failed elevated COM activation in both
architectures. Elevated COM ignores per-user classes; the playbook now explicitly
uses private HKLM keys only on elevated disposable GitHub Windows runners, with
collision checks in both hives, context verification and exact cleanup. Local
tests remain HKCU; unsupported machine-fixture invocation is rejected. Updated CI must pass
before closing the import validation gate. This does not close milestone 3's
complete frontend gate or the remaining original milestones.

### 4. Native ASCOM outputs and broader republishing

- [ ] Add native ASCOM hub outputs with the shared setup styling and descriptors.
- [ ] Preserve dynamic output identity and selection across registration/upgrades.
- [ ] Extend typed proxy coverage to focusers, rotators, filter wheels, flat panels,
  and cameras in separately reviewable increments.

Focuser controller increment: shared leases, bounded connection readiness,
generation-bound sessions, strict live capabilities, per-operation control,
absolute/relative limits, motion/temperature properties and uncertainty/cancellation
are implemented in Rust. Private actor/HTTP tests and EAF/FC3/ETA production-worker
simulation pass. Native/Alpaca focuser runtime admission, typed private IPC,
deduplicated polling and cached typed diagnostics/DeviceState are implemented.
All three frontend publications, Windows COM imports and virtual inputs are
implemented. Dedicated focuser simulation is implemented. Broader simulation, general typed
setup and conformance remain required.
Shared setup now enables Focuser, Rotator, FilterWheel and CoverCalibrator proxy creation, with other proxy classes
gated until their interfaces are implemented and verified.

Rotator controller increment: shared typed sessions and live property/command
semantics are implemented with eleven private actor/Alpaca V3/V4 cases. Native
CAA/Falcon reference persistence, adapter coverage and native/Alpaca runtime/IPC
are implemented. Alpaca, native NINA and native ASCOM publication are implemented
and pass local private-fixture checks. NINA verifies accepted targets and completion
without replay or implicit Halt. Virtual inputs reuse bounded typed connection
admission and preserve cached ages/errors through the validated graph.
Windows COM imports reuse the existing isolated STA worker and typed polling,
with V2/V3 legacy and V4 asynchronous ownership. Both-architecture private worker
and registered parent tests pass. Shared rotator creation is implemented.
Dedicated rotator simulation now uses shared timed
motion, atomic controls and retained uncertainty through all three outputs.
Conformance and broader acceptance stay open.

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

2026-10-07: Recovery checkpoint CI completes with seven successful jobs and one
Windows failure in both PR/push runs. The new timing trace exposes a private HTTP
fixture scheduling delay; an isolated occupied-pool baseline reproduces its read
timeout. Dedicated bounded workers pass eight replies and accepted partial-client
cleanup under complete shared-pool saturation in x86/x64. Final NINA 229/229 and
all actual net48 clients pass. Keep ETA cancellation timing and distinct connection
failures open pending new CI; no assertions, product deadlines or retries changed.
Publish this reviewed fixture refinement with factory ebcb704 to draft PR #21,
then continue native timing/runtime, every camera input/output, coordination and
all original final gates.

2026-10-07: Reviewed native camera factory and host resource integration. One
budget/activity counter survives configuration revisions; retained readers remain
charged and capacity rejection precedes exposure dispatch. Actual SDK/direct
simulations and a two-client executable scalar gauge pass. Full core/hub/Alpaca/
ZWO Rust suites, strict Clippy, Rust 1.89, contract freshness, Node/eight schema
checks, NINA 229/229 and actual net48 x86/x64 pass. Retain the initial executable
lock build failure and default-Python dependency failure; serialized Rust and the
existing schema venv pass. Preceding recovery head 6f29557 CI is still live, so
keep this increment local. Next: core-derived native timing/cleanup and runtime
supervision, remaining camera inputs, bounded IPC/all outputs, coordination and
every original acceptance/documentation/final gate. Camera choices stay disabled.

Native recovery metadata checkpoint (2026-10-06): migrated Rust recovery
declarations and added strict hub native-camera configuration without changing
legacy sparse defaults, unknown-extension loading or accepted numeric ranges.
Full validation and the first failed NINA run's deterministic cache-transition
diagnosis are recorded in hub-review.md. Core worker behavior/deadlines are
unchanged. All original remaining runtime, camera output, coordination,
conformance/acceptance, documentation and final audit gates remain required.

Imaging-control checkpoint (2026-10-06; reviewed locally, new CI required): gain and
offset share core's cooler write/readback helper, absolute deadline and persistent
framed-write uncertainty tracking. Only acknowledged values are published or
restored. Native owner tasks retain activity/readback/retirement after dispatch,
skip queued abandoned/expired work, block conflicting operations/publication and
reject old-generation buffered ACKs. The previous immutable image remains valid.
Four new core cases and twenty-one owner integration cases pass; nine native
unit cases include dropping the actual caller after write ACK before readback.
Full Rust regressions, strict Rust 1.99 Clippy, Rust 1.89 all-target checks,
contracts/Node/seven schema cases, rebuilt-host NINA 228/228 and real net48
x86/x64 clients pass. Review records the late-readback deadline correction and
the test-only MutexGuard lint failure/fix, with all five owner unit cases passing
again. Next: native adapter/config/factory/runtime with host-wide memory/activity,
recovery allowances and truthful cached observation ages, then remaining camera
inputs, all outputs, coordination and every original remaining gate.
Preceding a92b8bd PR CI is terminal: seven successes, one macOS lease-cleanup
assertion failure; push has seven successes and the retained Windows initial
connection failure. All original remaining gates are still required.

Latest property checkpoint (2026-10-06): native RAW16 properties use acknowledged
core state and immutable frame timing. Desired symmetric bin/ROI settings preserve
intermediate combinations, freeze atomically at capture admission and cannot
change active work. Four property unit and eighteen production-worker simulation
owner cases pass, including all shared properties on SDK/cooled direct cameras,
uncooled capabilities, malformed metadata, missing timestamps, uncertainty and
unchanged retained readers. Full Rust, strict Clippy/MSRV, generated contracts,
Node and seven independent schema checks pass. The first schema invocation used
system Python without jsonschema; the existing private schema venv passes. The
initial 12/14 owner run exposed clamp fixtures rejecting the new initial refresh;
they now accept the initial -10 target and reject only the intended later -15
request. Fresh-host NINA 228/228 and real net48 x86/x64 fixtures pass with no build
warnings; the reviewed checkpoint remains local while preceding CI is live.
Next: acknowledge native
gain/offset writes, then adapter/config/runtime and all original later gates.

Latest camera owner checkpoint (2026-10-06): retained SDK/direct cooler commands
share the capture engine and survive caller loss after dispatch. Pending work
blocks publication; reset rejects even buffered old-generation ACKs. Unknown
outcomes fence further work until explicit reset/close, preserving existing image
readers. Fourteen owner integration, three owner unit and seven core mailbox cases
pass, with full Rust regressions, strict Rust 1.99 Clippy, Rust 1.89, contracts,
rebuilt-host NINA 228/228 and net48 x86/x64 clients. Review retains fixture setup,
paused-clock and lint evidence. Preceding 6584e67 PR/push CI passes all eight jobs;
the reviewed core/owner increments now require their own CI. Next: native typed
camera properties/settings, source adapter/config/runtime and recovery/budget
wiring, then remaining adapters, all three image outputs, coordination and every
original acceptance/final gate. Camera choices remain disabled.

Earlier native panel checkpoint (2026-10-06): NINA IFlatDevice and native ASCOM
CoverCalibrator V2/V1 reuse shared protocol validation, private sessions,
saved identities, setup styling and registration. NINA waits for actual cover
endpoints and illumination completion, preserves logical On(0), bounds waits and
never invents compensating commands. ASCOM remains nonblocking and preserves
Int32/enum/Boolean DeviceState types. Private V1/V2 upstreams verify siblings,
live limits, absent components, modern errors, cancellation and lost replies.
An actual Alpaca process shares a panel with NINA; stopping it leaves NINA's
lease and illumination intact. Real net48 x86/x64 tests and eight-output manual
COM exports with both client bitnesses pass. Cold/production registration still
requires CI; interactive/hardware acceptance is separate. This increment stays
locally verified with full Rust hub/Alpaca suites, warnings-denied NINA 223/223,
both-architecture staging/client checks, Node and six schema cases. Review records
the corrected fixture enum assumption and preserves its failure evidence.
This increment stays
local while preceding 36a5558 CI runs. Next: panel COM/virtual inputs, dedicated
simulation/shared creation, cameras/coordination and every original later gate.

Previous HTTP checkpoint (2026-10-06): panel Alpaca V2 publication reuses the existing
publisher, private IPC and typed controller. Five new actual HTTP cases cover
dynamic identities/numbers, independent clients/sources, V1/V2 inputs, live
brightness bounds/zero-on, component absence, known stopped/unknown endpoint,
cached DeviceState omission, slot-zero collision/setup scoping and applied-once
uncertain commands without replay or cleanup actuation. Production OFP2 uses
explicit simulation. All 37 router cases, full Rust hub/Alpaca regressions,
strict Clippy, Rust 1.89 all targets, contracts, Node/six schema checks,
fresh-host NINA 214/214 and net48 x86/x64 pass. Standalone OFP2 worker/HTTP
simulation also passes. No physical equipment or installed vendor driver was
opened. Preceding 5d0ed34 PR/push CI now passes all eight jobs in both runs;
these reviewed panel increments are pushed at 36a5558 and their own CI is running.
Next: native NINA/ASCOM panel publication, COM/virtual inputs, dedicated
simulation/shared creation, cameras/coordination and every original remaining gate.

Previous runtime checkpoint (2026-10-06): panel runtime/IPC/cache reuse the existing host,
polling, typed observation envelopes and diagnostic displays. Nineteen panel,
eight factory and twenty-two native cases pass in full Rust hub/Alpaca regressions.
Actual Alpaca V1/V2 runtime polling and production OFP2 simulation retain sibling
leases, actual state, live brightness dependencies, independent errors, ages and
write uncertainty. DeviceState omits unavailable fields without I/O. Strict
Clippy, Rust 1.89, generated contracts, Node/six schema checks, fresh-host NINA
214/214 and real net48 x86/x64 pass. Retain the first fixture compile failures and
partial-poll assertion evidence: MaxBrightness had not yet arrived, so Brightness
was correctly unavailable. The fixture now waits for that dependency within its
unchanged deadline. This increment stays local while preceding wheel CI finishes.
Next: panel Alpaca/native NINA/ASCOM publication, imports/virtual inputs, dedicated
simulation and shared creation, then cameras/coordination and every original
remaining acceptance/final gate.

Previous controller checkpoint (2026-10-06): the panel controller reuses shared typed sessions
and operation leases. Fifteen controller tests, including real loopback Alpaca
V1/V2, cover absent components, strict live brightness, zero-on, actual warm-up,
known/unknown completion, cancellation, independent leases and uncertain applied
commands without replay. Both native panel cases pass with SIM-OFP2. Ten vendor
protocol cases and the timeout unit test pass. Full Rust hub/Alpaca, strict Clippy,
Rust 1.89, generated contracts, Node/six schema checks, fresh-host NINA 213/213,
real net48 x86/x64 and standalone OFP2 worker/HTTP simulation pass. Review and
logs are recorded in hub-review.md. This panel increment is local while preceding
5d0ed34 PR/push CI 37547703480/37547695748 runs. Next: panel runtime/IPC/cache,
all publications, COM/virtual inputs, dedicated simulation and shared creation;
then cameras/coordination and every original remaining acceptance/final gate.

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
| 2026-10-05 | Added shared cached DeviceState and supervised asynchronous connection changes, then exposed Switch 3, SafetyMonitor 3 and ObservingConditions 2 through capability negotiation. Retained asynchronous failures until explicit reconciliation and preserved separate client leases. | 181 Windows hub tests plus the endpoint fixture and 29 Alpaca tests pass, with Clippy, Rust 1.89.0, schema freshness and fresh package verification. No hardware was actuated. Next: native frontend attachment, conformance and remaining setup refinements; original milestones 2–5 stay open. |
| 2026-10-05 | Added the shared .NET attachment/IPC client for native NINA and ASCOM, using the existing frontend assembly and Rust host helper. Reviewed pipe permissions, cancellation, unknown operations, bounded buffers, terminal errors and finalizer lifetime. | 73 NINA regression/contract tests pass, including 32 new hub-client checks; real net48 x86/x64 attachment/lease tests and warnings-denied builds pass. Fixed a shared-intermediate bitness cache exposed by the runtime fixture. Scalar checkpoint bb55313 passed both complete CI runs. Next: actual native NINA providers/setup and all original remaining gates. |
| 2026-10-06 | Added native NINA Switch, SafetyMonitor and ObservingConditions providers, saved output identities, and a shared native output selector. Reviewed cancellation, stale objects, write/readback uncertainty, capability failures and selection-store conflicts. | 88 NINA tests pass, including 15 new native production-host/fault cases and actual NINA-adapter/Alpaca-publisher sharing. net48 x86/x64 selection/session/reconnect fixtures and warnings-denied builds pass. Both CI runs for client checkpoint 9006a99 passed completely. Native descriptor editing, interactive NINA acceptance, COM imports and all original remaining gates stay open. |
| 2026-10-06 | Added the shared native descriptor editor, review/apply/reconciliation session and cached health. Corrected described scalar choices in both frontend readers, preserved invalid input/identities, fenced disposed sessions and reviewed uncertain saves. | 109 NINA tests, real net48 x86/x64 editor fixtures, warnings-denied builds, 29 Alpaca tests, JavaScript and four independent schema checks pass. An automated WPF workflow supplies verified simulation renders; Chrome verified editing/reviewing Connection Policy without equipment leases. Native-provider PR CI passed; the push fixture timing correction and this editor require new CI. Next: shared setup refinements, interactive NINA, isolated COM imports, native ASCOM and all remaining original milestones. No complete milestone 2–5 gate is closed by this checkpoint. |
| 2026-10-06 | Added the isolated x86/x64 Windows COM import-worker boundary for Switch, SafetyMonitor and ObservingConditions. Reviewed STA/pump, connection ownership, terminal framing, sanitized HRESULTs and uncertain command/cleanup behavior. | Both warnings-denied builds and 16 real registered-COM cases in each architecture pass. Fixture registration is removed; hardware remains untouched. Native-editor 2171bec passed all eight jobs in both CI runs. Next: Rust COM adapter/factory integration, parent process ownership/deadline/generation and actual shared-policy tests, private worker packaging/signing, then all original remaining gates. No COM capability is advertised yet. |
| 2026-10-06 | Adopted scalar COM workers in the Rust factory; shared Alpaca/COM sampling, typed reply validation, process ownership, capability metadata and private Windows packaging. Reviewed cancellation, lost writes, connection/cleanup uncertainty and vendor helper isolation. | Ten real Rust-parent cases run in both bitnesses, plus 16 worker cases; Clippy, core/hub/Alpaca tests, Rust 1.89, 110 NINA tests and net48 x86/x64 fixtures pass locally. Both unsigned packages validate. Foundation CI failed Windows activation (0x80070002); loader diagnostics and updated CI remain required. Next: resolve that failure, native ASCOM outputs and shared setup refinements, then all original acceptance/coordination/documentation gates. |
| 2026-10-06 | Diagnosed elevated runner COM activation after successful direct managed loads; selected private HKLM fixture keys explicitly only for elevated disposable GitHub runners. Added native saved-choice management using the common store/selector. | Adapter 9dfdb82 passed seven jobs in both CI runs but Windows still failed; the runner correction requires new CI. Local 16 worker and ten parent cases pass in both bitnesses; machine registration is rejected outside runner context. All 114 NINA checks, net48 x86/x64 removal/lease fixtures and warnings-denied shared builds pass. WPF render verified after correcting clipped identity/path text. Next: verify corrected CI, native ASCOM outputs and remaining original gates. |
| 2026-10-06 | Added typed native ASCOM scalar adapters and bound export factories in the existing helper; shared the COM server with serial frontends. Reviewed capability admission, private leases, response epochs, typed errors and simulation metadata. | 117 NINA tests, net48 x86/x64 adapters, four private COM outputs against both server/client bitnesses, existing serial COM regressions and import-parent tests pass locally. Checkpoint 64178eb passed both complete CI runs; a newer queued-writer fixture deadline is corrected. Native chooser registration/removal, self-proxy aliases, SCM/setup/conformance and all original remaining gates stay open. |
| 2026-10-06 | Rejected canonical and aliased native ASCOM self-proxies before activation; moved the stable .NET identity helper into the common frontend assembly. Reviewed actual registry binding versus managed Type.GUID. Investigated failed native-output CI and staged Unix socket permissions before publication; improved Windows peer diagnostics. | 17 worker and 11 Rust-parent cases pass in both bitnesses; 117 NINA checks, net48 fixtures, real exported COM and unsigned packages pass. Rust hub/Alpaca suites, Clippy and Rust 1.89 pass locally. Portable socket regression awaits CI (local cross-check lacks a Linux C compiler). Windows export timeout remains unresolved. Next: new CI, registration/removal, then every original remaining gate. |
| 2026-10-06 | Verified private socket publication on all four portable CI platforms; collected both Windows peer failures and fixed property-helper collection enumeration. Added explicit binding/host launch paths, startup phase diagnostics and private cold SCM fixtures, with process creation-time/image verification and exact cleanup. | f94c85c push 37447788480 and PR 37447794795 pass seven jobs but fail Windows DeviceState assertion. Both local property-helper bitnesses and bound COM exchange/Count/item tests now pass, with invalid paths/duplicates rejected and inherited paths ignored. Cold per-user activation still fails before startup; disposable-runner machine activation is pending. Next: verify corrected CI and SCM, registration/removal, then all original remaining gates. |
| 2026-10-06 | Verified d51abac passes both full CI runs, including strict COM collections and x86/x64 cold SCM activation. Added shared production registration/removal backend and CLI, per-output ownership inventory, same-user activation, selection locking, collision/version protection, bounded rollback with custom DACL preservation, and installer inventory cleanup. | 126 NINA tests (nine private registry cases), net48 x86/x64 fixtures, warnings-denied helper build and unsigned ASCOM package pass locally. Actual production helper fixture awaits new disposable-runner CI. Next: themed registration manager, installer lifecycle, conformance and every original remaining gate; no full native setup or broader milestone is closed. |
| 2026-10-06 | Added themed ASCOM registration manager through the existing setup executable and Start menu, shared native selector/editor, immutable elevation requests, orphan inventory removal, owner/install/version protection and explicit reconciliation after failed/unknown edits. Expanded nested helper in-use checks; identified the missing Inno dynamic-inventory uninstall hook. | 129 NINA tests including three WPF registration workflows, net48 x86/x64 fixtures, warnings-denied helper build and unsigned ASCOM package pass. Actual simulation render inspected and documented. Installer compilation/lifecycle and real UAC/Chooser/conformance acceptance remain pending; implement the uninstall hook before closing that gate. All original broader milestones remain required. |
| 2026-10-06 | Corrected registered-export fixture ordering after CI proved an uninitialized cleanup list; restored preflight and finally coverage. Wired Inno pre-uninstall inventory cleanup before file removal, added whole-install preflight/batch rollback and future foreign-schema preservation, and added actual installer lifecycle fixtures with private identities and a second helper install. | 131 NINA tests pass; final 11 registry checks, net48 x86/x64 fixtures, unsigned package and hooked installer compilation pass. Python/PowerShell syntax and local machine-fixture rejection pass. b76d2fc CI fails Windows fixture before publication; seven other jobs pass. New production/lifecycle CI remains required, alongside all original remaining gates. |
| 2026-10-06 | Extended installer acceptance to hold a private installed COM metadata object in PowerShell 7, verify nested helper busy guards and intact inventories without host startup, then release only its own RCW and await natural retirement before maintenance. | Python/PowerShell syntax pass. The new installed metadata/in-use case awaits machine CI; it does not actuate equipment or close the broader frontend acceptance gates. |
| 2026-10-06 | Added shared native credential setup, host-described protection/input/reference fields, caller-chosen immutable references and explicit lost-reply reconciliation. Review added transaction admission to status reads and revoked stale configuration reviews after credential mutations. Fixed installer prerequisite fixture handling of absent values and preserved original value kinds through explicit writable handles. | 136 NINA tests, full Rust hub/Alpaca suites, strict Clippy, Rust 1.89 compatibility, net48 build and x86/x64 fixtures pass. Actual WPF credential render inspected. CI e728f55 proves helper publication/SCM/removal but fails the initial installer platform-value read; the corrected lifecycle fixture needs new CI. Next: web credentials and native setup refinements, then all original remaining gates. |
| 2026-10-06 | Added web credential controls using shared host descriptors and strict public response shapes. Implemented separate setup connection and explicit same-origin Reload, preserving the catalog and output leases with bounded admission and shutdown fencing. Corrected the host-capacity fixture to account for both private streams. | JavaScript credential state tests, full Alpaca suites, strict Clippy, Rust 1.89 checks and 136 NINA regressions pass. Browser proves secret clearing, review invalidation, retained reference, status after own host restart, removal, no console errors and no equipment leases; actual screenshot inspected. 2a26281 PR CI passes all eight jobs, including actual installer lifecycle/installed metadata/in-use checks; interactive/conformance/vendor acceptance remains required. Next: shared native setup refinements and every remaining original milestone gate. |
| 2026-10-06 | Added shared native explicit inspection with host-described page parameters/deadline, saved-source selection, temporary lease ownership, checked revisions/cursors, no probe replay, and observed diagnostics export excluding configuration/credentials. Review preserved the preview when local invalid input sends no request. | 144 NINA checks, warnings-denied shared builds and real net48 x86/x64 inspection/export fixtures pass; the actual WPF render was inspected. Both 2a26281 CI runs pass all eight jobs. a2cad0a CI is still active. Next: shared initialization/simulation controls and broader diagnostics, then every original remaining milestone and acceptance gate. |
| 2026-10-06 | Added shared native/web simulation controls from host descriptors, sparse saved-source updates, revision fencing, current-state reset, absent weather sensors and class-specific faults. Fixed numeric Switch decoding through tagged IPC and reviewed malformed replies as uncertain without replay. | Full hub/Alpaca tests, strict Clippy, Rust 1.89, generated-contract freshness, JavaScript/four schema checks, all 149 NINA tests, warning-denied net48 and real x86/x64 clients pass. Actual WPF/browser renders are documented as simulation; browser verifies preserved sibling readings, unchanged config, no leases/errors. Both a2cad0a and 94b9adc CI runs pass. Next: simulation CI, initialization and broader diagnostics, followed by all original remaining gates. |
| 2026-10-06 | Added common first-time persistence and explicit --hub-init in the existing executable: fresh empty identities, flushed no-clobber publication, existing-file preservation and committed durability uncertainty before any worker/SDK/host initialization. Reviewed CLI mode admission and the actual persistence implementation. | Eight-creator race and invalid/relative/missing-parent cases pass; production CLI verifies no live endpoint/host ownership and unchanged existing bytes. Full hub/Alpaca suites, strict Clippy and installed Rust 1.89.0 pass. Unix symlink/portable CI remains required. Next: native create-file UI using this path with retained filename/unknown-result reconciliation, broader diagnostics, and every original remaining milestone. |
| 2026-10-06 | Adopted first-time creation in the shared native NINA/ASCOM selector via the existing bounded helper. Added retained copyable filename, serialized creation/read admission, unknown-result fencing and explicit file identity/absence reconciliation. Empty configurations load into the editor with no output selection. Reviewed two Windows CI fixture races without raising deadlines. | All 155 NINA checks, real net48 x86/x64 creation/reconciliation/ASCOM fixtures and warning-denied build pass; the actual WPF private lost-reply render was inspected. 6f36ff5 CI passes seven jobs but fails separate pipe fixtures; corrections need new CI. eeb9208 CI remains active. Next: verify CI, broader source/output/policy diagnostics, typed devices/cameras/coordination and every original remaining gate. |
| 2026-10-06 | Added shared native/web output health and observed export using the host-generated serialized reply schema. Reviewed required nullable fields, reference constraints, decoded string equivalence, saved identities/paging, uncertainty and review preservation. | Full hub/Alpaca suites, strict Clippy, Rust 1.89, generated-contract freshness, Node/four schema tests, all 168 warnings-denied NINA checks and actual net48 x86/x64 clients pass. Browser proves all three output classes, pagination, preserved review, downloaded Reviewed export, zero leases/errors; WPF/browser screenshots inspected. de2691a CI passes seven jobs but Intel macOS hits the outer 15-minute budget during otherwise passing tests. Budget increased to 25 minutes; complete new CI remains required. Next: actual actor retry scheduling, previous COM fixture timeout, and every original remaining milestone/acceptance gate. |
| 2026-10-06 | Added actual actor polling phases, observation-time waits, started attempts/cycle completion and backoff diagnostics to cached source/output health, generated schema, native/web summaries and exports. Reviewed in-flight visibility, delayed dispatch and command no-replay semantics. | Four paused-time scheduling cases, full Rust hub/Alpaca suites, strict Clippy, Rust 1.89, generated-contract/Node/four schema checks, all 171 NINA tests and real net48 x86/x64 clients pass. Actual WPF/browser simulation renders inspected; browser preserves one independent lease and cleanup confirms zero. 96ea4e3 CI passes seven jobs including Intel macOS; Windows still live. Next: accessory-only HTTP startup with explicit empty camera profiles, prior COM timeout and every original remaining gate. |
| 2026-10-06 | Completed accessory-only HTTP refinement: explicit empty camera lists, ordinary missing-file defaults, common startup/reload identity checks and empty camera setup with first-slot creation. | 15 Alpaca unit, nine executable and 14 router tests pass; strict Clippy/Rust 1.89, formatting and JS syntax pass. Browser proves empty/add/save flow, unchanged hub catalog and zero leases/connections/errors; simulation screenshot inspected. Both 96ea4e3 CI runs pass all eight jobs. Next: polling CI, earlier COM timeout and broader typed proxies/camera ownership/coordination plus every original remaining acceptance gate. |
| 2026-10-06 | Reviewed polling CI Windows failure at native safety-expiry unchanged-generation assertion. Changed the fixture to establish a completed 503 with Retry-After longer than the safe lifetime before checking expiry, retaining generation, stale state, weather, timing and recovery assertions. | All 172 NINA tests and five focused expiry repeats pass. Production deadlines/policies unchanged; exact old reset cause remains unproved. New CI required. Next: corrected CI and broader typed accessory proxies, retaining COM timeout and all original remaining acceptance gates. |
| 2026-10-06 | Added and reviewed the typed focuser controller over shared source actors. Live capability/range checks, actual readiness, generation fences and unique command control preserve ownership and uncertainty. Native EAF/FC3/ETA adapters reuse existing workers and known units. | 13 private actor/HTTP cases plus eight production-worker simulation cases pass; full hub/Alpaca suites, strict Clippy and Rust 1.89 checks pass. Review corrected absolute per-move travel and retained ETA's known 1 µm coordinate. Empty-profile push CI passes all eight jobs; safety-correction push/PR each pass seven with Windows still live. Next: focuser runtime/IPC/diagnostics and all three frontends/imports, then the remaining typed devices/cameras/coordination and every original acceptance gate. No typed proxy is advertised yet. |
| 2026-10-06 | Integrated focuser outputs into native/Alpaca source runtime and typed private IPC. Added deduplicated typed polling, inert paged diagnostics, host-described property types/ranges, strict web/native readers and cached DeviceState. Reviewed generation races, pending admission, EOF, class dispatch and unsupported frontend publication. | 18 focuser actor/HTTP/runtime/IPC cases and nine native cases pass; full hub/Alpaca suites, strict Clippy, Rust 1.89, generated contract, Node/four schema tests, 173 NINA checks and real net48 x86/x64 clients pass. Final focused checks pass after fixture refinements. Safety-correction CI passes all eight jobs in both runs; a83e577 CI each pass seven with Windows live. Next: Alpaca and native NINA/ASCOM focuser publication, typed imports/simulation and every original remaining gate. Setup's general proxy capability stays unavailable. |
| 2026-10-06 | Published typed focuser outputs through Alpaca with sparse identities, modern interfaces and shared IPC; reviewed class-local collisions, private lease ownership, strict parameters and lost-write no-replay behavior. Local slots coexist at distinct numbers and per-device setup selects the shared editor. | Four production-adapter/host/HTTP loopback cases, full local Rust hub/Alpaca suites, strict Clippy and Rust 1.89 pass. No physical hardware is actuated. Controller push CI passes all eight jobs; its PR was cancelled. Runtime/IPC push/PR each pass seven with Windows live. Next: native NINA/ASCOM focuser publication and broader imports/simulation, then every original remaining typed-device/camera/coordination and acceptance gate. General proxy setup remains gated. |
| 2026-10-06 | Added native NINA/ASCOM focuser outputs using shared typed request/value helpers, immutable selection and Focuser Chooser registration. Reviewed cancellation, generation checks, typed DeviceState and export mapping. | 180 NINA tests, warnings-denied builds, real net48 x86/x64 adapters and manual COM exports pass. Local SCM fails on the first existing Switch class; updated SCM/production registration requires disposable CI. Runtime/IPC PR CI passes all eight jobs. Next: typed imports/simulation and general typed setup, then remaining devices, camera ownership/coordination and every original acceptance gate. |
| 2026-10-06 | Added Windows COM focuser imports through the existing isolated STA workers and shared typed controller. Review corrected both sides of the V3 legacy/V4 asynchronous boundary, preserved Int32 limits, shared leases and uncertain-write fencing. Corrected the production-registration fixture's missing Focuser Chooser path. | 20 actual worker tests, 12 registered Rust parent cases, full Rust hub/Alpaca suites with production workers in explicit simulation, strict Clippy, Rust 1.89, 180 NINA tests and real net48 x86/x64 clients pass. Native frontend CI proves cold SCM activation for all five classes but fails the now-corrected registration mapping; new CI required. Next: typed virtual/simulated focuser inputs and general typed setup, then every original remaining gate. |
| 2026-10-06 | Added virtual focuser composition through the shared typed controller, generation-checked cached polling and incremental supervised inner connections. Reviewed ages, types, optional errors, ownership, pending cancellation and no replay/automatic Halt. | Seven private loopback cases and one explicitly simulated production EAF worker case pass, alongside full Rust hub/Alpaca suites, strict Clippy, Rust 1.89, 180 NINA tests and real net48 x86/x64 regressions. COM checkpoint CI is still active. Next: dedicated focuser simulation and shared typed setup, other accessory proxies, cameras/coordination and every original acceptance/final gate. |
| 2026-10-06 | Added and reviewed dedicated Focuser V4 simulation through shared actors/controllers and fifteen generated native/web controls. | Fifteen Rust simulation cases, full hub/Alpaca suites, Clippy/MSRV, Node/four schema checks, all 184 NINA tests and real net48 x86/x64 fixtures pass. Actual WPF screenshot is labelled simulation. COM push CI 37494616586 passes all eight jobs. Next: shared typed setup; all original later gates remain required. |
| 2026-10-06 | Enabled shared class-gated Focuser setup, including Windows COM choices; fixed the browser tagged-type event closure found in actual acceptance. | Native review/apply creates two shared outputs without source leases. All 186 NINA tests, real net48 x86/x64 fixtures, Rust/Clippy/MSRV, Node form-event tests and Chrome creation/save/reload/sparse Int32 acceptance pass. New screenshot shows explicit simulation. Next: other typed accessory proxies and every original remaining gate. |
| 2026-10-06 | Added typed rotator semantics and extracted shared accessory session ownership from the focuser controller. Reviewed generation, cancellation, independent angles, optional errors and no replay. Added evidence to a newly observed Windows initial focuser-connection failure without retries/deadline changes. | Eleven private actor/actual Alpaca V3/V4 cases, full hub/Alpaca regressions, Clippy/MSRV, contract/Node/four schema checks, 186 NINA tests and real net48 x86/x64 clients pass. Rotator publication remains gated. Next: native reference persistence and typed adapters, then runtime/IPC, all frontends/imports/setup and every original remaining gate. |
| 2026-10-06 | Added and reviewed native CAA/Falcon typed properties and durable reference recovery. Uncertainty markers, strict readback and revision-fenced commits preserve unknown Sync outcomes; reconnect restores only local mappings. Fixed stale CAA direction observation and Sync target reporting. | Five Windows storage cases, thirteen native cases, eight CAA controller/fourteen protocol/eight Falcon tests, final Rust/Clippy/MSRV/contracts, Node/four schema checks, all 186 NINA tests and real net48 x86/x64 clients pass. Portable permissions coverage needs CI. Prior controller PR CI passes all eight jobs. Next: rotator runtime/IPC, all frontends/imports/setup and every original remaining gate. |
| 2026-10-06 | Integrated native/Alpaca rotators into shared runtime, typed IPC, deduplicated polling and cached diagnostic contracts. Reviewed generation/cancellation/EOF, standard DeviceState membership and combined typed/scalar sample limits. Native/web readers share host fields and bounds. | Fifteen rotator cases, fourteen native cases, exact-limit factory regression, full final Rust/Clippy/MSRV/contracts, Node/four schema tests, all 187 NINA tests and real net48 x86/x64 clients pass. Reference checkpoint PR CI has seven successes with Windows live. Next: Alpaca/native NINA/ASCOM rotator publication, imports/simulation/setup and every original remaining gate. |

| 2026-10-06 | Published rotators through the common Alpaca typed IPC adapter and reviewed modern reversal readiness, shared ownership, local-slot coexistence, command routing and sanitized errors. | Seven HTTP cases (including actual CAA/Falcon workers in explicit simulation), sixteen hub rotator cases, final rebuilt-worker/host Rust suites, Clippy/MSRV/contracts, Node/four schema checks, all 187 NINA tests and real net48 x86/x64 clients pass. Reference PR/push CI 37508673278/37508667983 passes all eight jobs. Runtime PR CI 37510990909 has seven successes with Windows live; push 37510983650 is still running. Next: native NINA/ASCOM rotators, imports/simulation/setup and all original remaining gates. |
| 2026-10-06 | Published native NINA/ASCOM rotators through shared typed request/value helpers and stable registration. Reviewed command ownership through relative target receipt, ignored-move detection, strict completion, cancellation, optional values and standard Single DeviceState. | Final Rust hub/Alpaca suites, seventeen rotator cases, Clippy/MSRV/contracts, Node/four schema tests, all 196 warnings-denied NINA checks, real net48 x86/x64 clients, both-architecture staging and six-output manual COM exports pass. Retained parallel compilation resource failure; single-job retry passes. Alpaca PR/push CI 37513525462/37513518205 is cancelled at the proven outer Windows 25-minute limit; increased only that budget to 45. Runtime CI is also cancelled, exact cause unverified. New CI/registered SCM acceptance required. Next: rotator imports/simulation/shared creation, then all remaining typed devices/camera ownership/coordination and original acceptance gates. |
| 2026-10-06 | Implemented virtual rotator inputs using shared typed admission, immutable generation checks and cached age/error forwarding. Extended existing loopback/native fixtures instead of duplicating them. | Final rebuilt-host Rust hub/Alpaca suites, twenty-two rotator cases, fifteen native cases, existing virtual regressions, Clippy/MSRV/contracts, Node/four schema checks pass. Rebuilt-host NINA run remains live with three reported failures; retain its log and investigate after terminal state. net48 is queued behind successful NINA completion. Frontend CI 37518077578/37518073024 has seven successes with Windows live. Keep this checkpoint local until CI completes and managed failures are resolved. Next: managed evidence, missing typed ASCOM ProgID coverage, then rotator COM/dedicated simulation/shared creation and every original remaining gate. |
| 2026-10-06 | Recorded terminal managed failure and corrected HTTP fixture isolation/cleanup reporting. | Rebuilt-host NINA ended 191/196 passed, five failed. A seven-case focused run passed six but reproduced publisher readiness failure. Publisher now uses private empty profiles and explicit simulation; compile passes but focused readiness still fails with empty captured output. No deadlines relaxed. Standalone private initialization succeeds in 5.87 s; other original failure causes remain unproved. net48 did not run. Next: diagnose publisher startup and helper timing, then resume all original remaining gates; keep checkpoints local while prior CI remains live. |
| 2026-10-06 | Verified terminal native-rotator frontend CI at pushed 63e7ae4. | PR/push 37518077578/37518073024 both pass all eight jobs, including Windows packaging/registered COM validation with the 45-minute outer budget. Local 632f612/6b298f5 virtual/fixture checkpoints still need managed acceptance before push. Next: reproduce and diagnose private publisher startup, then all original remaining gates. |
| 2026-10-06 | Confirmed virtual checkpoint managed tests after isolated CLI timing probes, preserving earlier failure evidence. | Worktree/Unicode temporary/helper-launched hosts with two IPC clients report publisher startup near 47 ms; isolated NINA publisher case passes in 820 ms. Full rebuilt-host NINA 196/196 and real net48 x86/x64 confirmation pass. No deadlines increased; earlier intermittent causes remain unproved and broader reliability acceptance stays open. Virtual checkpoint is ready for its own CI. Next: typed ASCOM ProgID coverage, rotator COM imports, dedicated simulation/shared creation and every original remaining gate. |
| 2026-10-06 | Implemented and reviewed Windows COM rotator imports through existing STA workers, typed controllers and leases. Corrected typed ASCOM identity cycle validation and aligned Rust/C# Single boundary checks. | Twenty-four private worker and fourteen actual registered parent cases pass, including existing scalar/focuser regressions; final Rust hub/Alpaca, Clippy, Rust 1.89, contract freshness, rebuilt-host NINA 196/196 and real net48 x86/x64 pass. Retain the first parent numeric-representation assertion failure; the corrected test compares strict numeric values. Prior virtual CI has seven successes with Windows installer tests live; let it finish before pushing. Next: dedicated rotator simulation, shared creation and every original remaining gate. |
| 2026-10-06 | Confirmed exact-head virtual checkpoint 87ca1c1 CI before pushing the reviewed COM increment. | PR/push runs 37522869618/37522861252 both pass all eight jobs, including Windows packaging, registered imports and installer acceptance. COM commit 89fae59 has complete local checks and now proceeds to its own CI. Dedicated simulation is in local implementation; shared creation and all original remaining gates stay open. |
| 2026-10-06 | Implemented and reviewed dedicated rotator simulation through the existing actor, typed controller, virtual graph and all three outputs. Shared setup derives twelve controls and strict nested/Single validation from host metadata. | Twenty-one simulation cases, full Rust hub/Alpaca suites, warning-denied Clippy, Rust 1.89, generated contracts, Node/four schema tests, rebuilt-host NINA 199/199 and real net48 x86/x64 pass. Actual WPF screenshot is explicitly labelled simulation. Retain the first temporary-lease assertion failure and Clippy enum-size diagnostic; fixes preserve deadlines and JSON. COM checkpoint PR/push CI 37526619358/37526612935 has seven successes with Windows installer acceptance live. Keep this increment local until that checkpoint finishes, then push to the same draft PR. Next: shared rotator creation, remaining typed devices/camera ownership/coordination and every original acceptance gate. |
| 2026-10-06 | Enabled and reviewed shared rotator creation, reusing generated native/web forms and installed COM worker/bitness choices. | Full Rust hub/Alpaca suites, Clippy, Rust 1.89, contracts, Node/four schema checks, all 201 NINA tests and real net48 x86/x64 created-output fixtures pass. Actual WPF and in-app browser acceptance cover creation/reload, class mismatch, sparse Single controls and zero leases; screenshots label simulation. Retain the obsolete Rust gate assertion, original-source-list WPF failure and empty-tab/early-visibility capture evidence. Preceding simulator PR/push CI 37530345334/37530337700 has six successes with Intel macOS and Windows live. Keep this increment local until CI finishes, then publish to the same draft PR. Next: typed wheels and panels, camera ownership/transport, coordination and every original remaining acceptance/final gate. |
| 2026-10-06 | Recorded simulator CI Windows lost-Move-reply failure and added shared failure-only dispatch/source evidence to focuser and rotator fixtures. | PR run 37530345334 ends with seven successes and one Windows failure; push 37530337700 remains live. Both focused cases and all 201 NINA tests pass locally. No assertions, retries, deadlines or production behavior changed; cause remains unproved. Next: new CI evidence, typed wheels/panels, cameras/coordination and every original remaining gate. |
| 2026-10-06 | Implemented and reviewed the typed wheel controller using shared sessions/leases; added bounded flat metadata arrays to the source cache after a reproduced array rejection. | All eleven private actor/actual Alpaca V2/V3 wheel cases, twenty-six actor cases, full Rust hub/Alpaca suites, Clippy/MSRV/contracts, Node/four schema checks, rebuilt-host NINA 201/201 and real net48 x86/x64 pass. Retain initial compile/preflight/cancellation failures; no deadlines changed. Simulator push CI 37530337700 passes all eight jobs; its PR failure cause remains open. Next: native wheel metadata/adapters, runtime/IPC and all outputs/imports/setup, then panels, cameras/coordination and every original remaining gate. |
| 2026-10-06 | Added native EFW metadata through existing workers and shared config descriptors; reviewed calibration, saved identity and low-level move admission. Fixed independently reproduced Int32 schema overflow and kept slot mismatch explicit. | Eighteen native and sixteen config cases, full Rust hub/Alpaca suites, Clippy, Rust 1.89, contract freshness, Node/five schema checks, all 202 NINA tests and real net48 x86/x64 pass. Retain wrong simulator-motion expectation, schema overflow and editor-label/unsaved-inspection failures. Exact bf15ced PR CI has seven successes but Windows x86 rotator Sync fails uncertain; cause unproved. Added failure-only stage/request/state/stack evidence, without deadline or behavior changes; its push remains live. Next: wheel poll plans/runtime/IPC, all three publications/imports/virtual/simulation/setup, then panels, cameras/coordination and every original final gate. |
| 2026-10-06 | Added shared typed array polling and deduplicated wheel plans. Factored cache/collected-result admission into SampleBudget after review identified aggregate array collection risk. | Twenty-seven Alpaca transport, seven factory, eleven wheel-controller and twenty-six actor cases pass; full Rust hub/Alpaca, strict Clippy, Rust 1.89, contract freshness, rebuilt-host NINA 202/202 and real net48 x86/x64 also pass. Strengthened collection tests verify one later request remains queued after the first over-budget value; focused confirmation passes. bf15ced push CI 37533737225 completes all eight jobs; its failed PR cause remains unproved. Native metadata head 630b302 is pushed and its PR/push CI remains active. Keep this verified polling increment local while continuing wheel runtime/IPC and all original remaining publication/acceptance/final gates. |
| 2026-10-06 | Integrated wheel runtime/IPC and cached diagnostics, sharing typed observation envelopes with focusers/rotators. Reviewed cancellation, generation/uncertainty, array dependencies, escaped response limits and frontend contracts. | Fifteen wheel, nineteen native and full Rust hub/Alpaca suites pass; Clippy, Rust 1.89, contract freshness, Node/five schema checks, rebuilt-host NINA 203/203 and real net48 x86/x64 pass. Strengthened cache/IPC confirmation proves per-key recovery, Position omission and stream survival after oversized diagnostics. Native metadata 630b302 PR/push CI 37536023962/37536015902 both pass all eight jobs. Next: all three wheel publications, imports/virtual/simulation/setup, then panels, cameras/coordination and every original acceptance/final gate. |
| 2026-10-06 | Published wheels through the common typed Alpaca adapter; factored setup-page routing with focusers/rotators. Reviewed dynamic identities, local slot coexistence/collisions, metadata/Position bounds and unknown-write fences. | Five new wheel and all 31 router cases pass, including production EFW simulation; full Rust hub/Alpaca, strict Clippy, Rust 1.89, contracts, Node/five schema checks, rebuilt-host NINA 203/203 and actual net48 x86/x64 pass. Runtime PR CI 37539206029 has six successes with Intel macOS/Windows live; push remains live. Keep this locally verified increment until CI completes, then push to the same draft PR. Next: native NINA/ASCOM wheel outputs, imports/virtual/simulation/setup, then panels, cameras/coordination and all original acceptance/final gates. |
| 2026-10-06 | Added and reviewed native NINA/ASCOM wheel outputs using shared protocol validation, selections and registration. Preserved NINA filter settings and profile-change admission; verified nonblocking actual Position and no-replay fences. Corrected the asynchronous rotator cleanup assertion exposed on Intel macOS and expanded private transport failure evidence. | Full Rust hub/Alpaca, strict Clippy, Rust 1.89, contracts, Node/five schema checks, fresh-host NINA 209/209, real net48 x86/x64, both-architecture staging and seven-output manual COM exports pass. Retain first headless collection failure and cold HKCU SCM failure at existing Switch class; production/cold wheel registration needs CI. Runtime push 37539201271 fails Windows initial focuser Connect (cause unproved) and macOS cleanup assertion; PR 37539206029 has seven successes with Windows installer acceptance live. Keep verified wheel increments local until that run ends. Next: wheel COM imports, virtual/dedicated simulation/shared creation, then panels, cameras/coordination and every original final gate. |
| 2026-10-06 | Verified preceding runtime PR CI and pushed reviewed wheel HTTP/native publication at 0669339 to draft PR #21. Implemented and reviewed Windows COM wheel imports using existing STA workers, typed polling/controllers and bounded array validation. | Runtime PR 37539206029 passes all eight jobs; its separate push failures remain retained. New publication PR/push CI 37541898392/37541893308 is live. COM import checks pass all 27 private worker and 16 actual registered parent cases, full Rust hub/Alpaca, strict Clippy, Rust 1.89, contracts, Node/five schema checks, fresh-host NINA 209/209 and real net48 x86/x64. Retain wrong fixture HRESULT and initial parent array-rejection failures; fixes preserve deadlines and error semantics. Keep imports local until preceding CI finishes. Next: wheel virtual/simulation/shared creation, panels, camera ownership/transport, coordination and every original remaining gate. |
| 2026-10-06 | Added and reviewed virtual wheel inputs using shared typed connection supervision, controllers and cached metadata forwarding; extended actual EFW worker simulation through two nested outputs. | All 21 wheel and 20 native tests, full Rust hub/Alpaca suites, strict Clippy, Rust 1.89, generated contracts, Node/five schema checks, rebuilt-host NINA 209/209 and real net48 x86/x64 pass. Retain first cache classification failure; corrected assertions require original Permanent errors at every layer and valid metadata. Preceding 0669339 PR/push CI has seven successful jobs and successful Windows test.ps1 steps, including private SCM/production wheel registration; final build/installer checks remain live. Keep COM/virtual increments local until those runs finish. Next: dedicated wheel simulation/shared creation, panels, camera ownership/transport, coordination and every original remaining acceptance/final gate. |
| 2026-10-06 | Added and reviewed dedicated wheel simulation using shared actors, timed motion, bounded array controls and atomic metadata. Actual IPC verifies applied updates with oversized replies; both frontends require reload without replay. | Full Rust hub/Alpaca suites, 27 simulator cases, strict Clippy, Rust 1.89, contracts, Node/six schema checks, fresh-host NINA 212/212 and real net48 x86/x64 pass. Actual WPF capture verified. Retain initial schema/test/Clippy failures and corrected evidence. Preceding a586c76 PR/push CI remains live; keep this increment local. Next: shared wheel creation, panels, cameras/coordination and every original acceptance/final gate. |

| 2026-10-06 | Enabled shared wheel creation using generated capability choices and existing editors; parameterized rotator/wheel WPF and real net48 creation checks. Reviewed browser creation, mismatched classes, saved identities and names-only updates. | Full Rust hub/Alpaca, 22 wheel cases, strict Clippy, Rust 1.89, contracts, Node/six schema checks, fresh-host NINA 213/213 and real net48 x86/x64 pass. Corrected a test request-ID ordering error in a separate IPC capability test; no protocol rule changed. Actual native/browser captures and zero-lease status verified. Preceding a586c76 CI remains live; keep local increments until it ends. Next: panels, cameras/coordination and every original acceptance/final gate. |
| 2026-10-06 | Preserved worker-relative observation ages through core and native camera properties; separated acknowledged evidence from desired settings and shared the four-control apply/readback helper. | Full core/hub/Alpaca/ZWO regressions (47 core, 49 hub unit, 22 native owner, 98 ZWO library), strict Clippy, Rust 1.89, contracts, Node/seven schema checks, rebuilt-host NINA 228/228 and real net48 x86/x64 pass. Retain initial fixture errors and corrections. Preceding adfb9e2 PR/push CI both pass all eight jobs. This checkpoint needs its own CI. Next: native camera adapter/config/factory/runtime with shared host budget/activity/recovery allowances and preserved sample ages, then all remaining camera inputs/outputs, coordination and every original acceptance/final gate. |
| 2026-10-06 | Integrated and reviewed native camera inputs through actual SourceActor/CameraSupervisor with shared ages, immutable images, strict commands, retained telemetry/restoration and unknown-outcome fencing. Fixed publication and worker/logical-session races found in review and integration. | Eleven adapter cases plus full Rust regressions (49 core, 49 hub unit, 22 native owner, 98 ZWO library), strict Rust 1.99 Clippy, Rust 1.89, contracts, Node/seven schema checks, fresh-host NINA 228/228 and real net48 x86/x64 pass. Preserve initial fixture/deadline errors and paging-file exhaustion; final build uses two compiler jobs. Observation b038f8e PR/push CI each fail one Windows NINA case; cause remains unproved and original logs are retained with new fixture timing/pool diagnostics. Next: native factory/config/runtime and host budget/activity/recovery allowance wiring, remaining camera inputs/outputs, recurring Windows CI investigation, coordination and all original acceptance/final gates. Camera choices remain disabled; keep PR #21 draft. |
