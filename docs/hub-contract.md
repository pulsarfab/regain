# Hub contracts (implementation baseline)

This is the milestone 0 design for [the hub plan](hub-plan.md). Changes to these
contracts must be recorded in that plan and covered by compatibility tests.

Executable configuration examples: [mixed switch](../crates/regain-hub/examples/mixed-switch.json)
and [two-source safety](../crates/regain-hub/examples/two-source-safety.json).
Their addresses and identities are illustrative; loading/validating them does
not connect to hardware. Replace the example identities before eventual use.

## Ownership and hosting

`regain-alpaca --hub-host --hub-config ABSOLUTE_PATH` now owns hub configuration,
sources, polling, and policies without opening HTTP or UDP discovery. It accepts
`--workers DIRECTORY` and explicit `--simulate`; ordinary HTTP/stdio options are
rejected in this mode. It requires an existing valid configuration file and does
not connect sources until clients request outputs. Ordinary Alpaca HTTP mode can
now attach using `--hub-config ABSOLUTE_PATH`; native NINA/ASCOM adoption remains
pending against this same local host.
Existing direct camera/accessory frontend behavior remains compatible.

Use one host per canonical configuration path and operating-system user. Windows
uses a named pipe restricted to that user; Unix uses a user-only Unix socket.
Derive endpoint identity from the user and canonical path. An OS-held exclusive
lock arbitrates startup; only its holder can publish an endpoint. A second
launcher waits for the first host's bounded readiness handshake, rather than
starting another source owner. Never trust a PID/endpoint file alone as proof
that a host is alive. Local IPC is not an unauthenticated TCP listener.

Each IPC connection owns a client identity; identities supplied in request JSON
cannot impersonate another connection. Closing it releases that client's leases.
Each source connects once while referenced, and closes only connections the hub
owns. Multiple config files do not bypass a physical device's existing exclusive
ownership checks. Externally managed upstream connections are never disconnected
by the hub. Explicit motion/acquisition leases are separate from connection leases.

Commands use bounded per-source queues. A stalled source never holds a global
state/configuration lock. Cached reads and safety expiry remain responsive. A
transport failure after a write may mean the write happened: return an uncertain
outcome and do not blindly resend. Cancellation does not imply physical rollback.

## IPC and errors

The in-process output runtime now owns one controller set per configuration
revision. Host-created client identities have separate output connection maps;
another client's ID is never an input to connect/get/disconnect. Safety clients
share one active policy per output. The last connection releases that policy,
publishes unsafe to retained subscribers, and makes a later session require new
observations. Weather averaging settings belong to the shared output controller.

Connection setup reserves a per-client slot before awaiting source I/O. Duplicate
pending connects return busy; reconnecting an already-ready slot is idempotent.
Cancellation removes only its reservation, so it cannot remove a replacement
connection. Closing a client cancels pending connects and releases ready guards.
An in-flight command retains its output guard until it finishes; disconnect
does not imply command cancellation or physical rollback. No client/output map
lock is held across driver I/O, and cached safety expiry remains independent.

Runtime shutdown permanently stops client admission and revokes safety before
waiting for sources. Each source finishes bounded in-flight I/O, rejects queued
and later commands, attempts owned-connection cleanup, clears cached values,
and retains its terminal cleanup result. All sources drain concurrently; errors
from one cannot skip the rest. Repeated or resumed shutdown returns the retained
result without replaying Disconnect. An active-connection count of zero alone is
not proof of worker teardown. Configuration replacement must drain the old
registry before allowing the next runtime to open its sources.

These runtime contracts have local lifecycle/fault tests. Scalar framing and
dispatch now use this runtime through a host-supplied stream. Protected local
endpoints and OS ownership locks are implemented separately below. The executable
host and configuration replacement are integrated. A Rust client and attachment
helper are implemented below; frontend adoption and resume handling remain open.

Use the existing convention: little-endian 32-bit JSON length followed by UTF-8
JSON; responses may carry separately bounded binary image data. Version the hub
protocol independently (`version: 1`), correlate requests with IDs, bound frames,
and reject unknown versions/members. The handshake advertises host instance,
configuration revision, protocol version, and supported capabilities. Camera
payload limits/lifetimes are defined before camera proxy enablement.

Initial operations: `describeConfig`, `getConfig`, `validateConfig`, `applyConfig`,
`listDevices`, `connect`, `disconnect`, `get`, and `put`. Configuration mutations
include an expected revision. Errors contain a stable code, safe message, and
optional field path/source ID. Do not include credentials or arbitrary driver
exception text in exported diagnostics. Standard outputs translate these errors
to their interface's error conventions.

### Implemented scalar wire protocol

Each JSON message is prefixed by its unsigned 32-bit little-endian byte length.
Frames are limited to 1 MiB. The first request must be `hello`; request IDs are
positive unsigned integers and must strictly increase on that connection.
Responses may arrive out of order and carry the matching ID. Neither retries nor
ID reuse authorize replay: duplicate/older IDs close the connection.

```json
{"version":1,"id":1,"command":{"op":"hello"}}
{"version":1,"id":2,"command":{"op":"connect","output":"10000000-0000-0000-0000-000000000101"}}
{"version":1,"id":3,"command":{"op":"get","output":"10000000-0000-0000-0000-000000000101","property":{"member":"isSafe"}}}
```

Successful responses have `version`, `id`, and `result`. Failed operations have
`version`, `id`, and `error` containing a stable `code`, sanitized `message`, and
optional `upstreamCode`; configuration failures may include field-addressable
`fields`. Hello returns stable `instanceId`, per-host-process
`hostInstance`, `configurationRevision`, connection-owned `clientId`, frame and
concurrency limits, and the implemented operations/capabilities. Client IDs in
requests are rejected, rather than interpreted as another client's authority.

Currently implemented: describeConfig, getConfig, validateConfig, listDevices,
sourceStatus, hostStatus, connect, disconnect, changeConnection, and typed get/put for Switch, SafetyMonitor,
and Weather. `validateConfig` reports persisted configuration/relationship errors
with `scope: "configuration"`; it does not authorize durable apply or establish
live hardware capabilities. `getConfig` retains credential references for local
editing, but contains no credential values. The executable's persistent service
also advertises `applyConfig`. A read-only embedded runtime does not advertise it
and returns unsupported. Camera image transport remains a separate pending feature.

Requests start in arrival order but do not wait for earlier I/O to complete.
This lets legacy Disconnect cancel a pending legacy Connect while cached safety
reads remain responsive during another request. A supervised changeConnection
instead rejects overlapping legacy or modern changes as busy. There are at most eight in-flight operations
and one buffered input frame per stream. Overload, malformed/unknown fields,
unsupported protocol versions, and invalid request order close the stream.
No-argument commands also reject unknown fields. Invalid typed device operations
return errors while preserving the connection.

Default deadlines are five seconds for hello, a partial frame, or writing a
response, and thirty seconds for an operation. The host supplies these limits.
Idle established connections are allowed. A dedicated reader detects EOF during
in-flight operations; closing/cancelling the server task releases that client's
leases. A timed-out put reports uncertain and is never replayed. Serialization
has a bounded output buffer; an oversized response returns `responseTooLarge`
without corrupting the next frame. Protocol failures never echo request content.

Framing/fault checks use in-memory duplex streams and the full runtime. Endpoint
and separate-process tests also exercise hello/listDevices over actual local IPC.

The `asyncOutputConnection` capability advertises `changeConnection` with
`output`, `connected`, and `asynchronous` fields, plus typed `get` member
`connecting`. Each client admits one connection change before spawning its task;
the reservation counts toward apply quiescence immediately. Asynchronous admission
returns null without waiting; synchronous callers await the same supervised task.
It has its own 30-second deadline, survives a lost waiter, and retains failure for
Connecting until an explicit connection action. EOF closes the client and cancels
pending connection reservations. No accepted change is replayed.

`scalarDeviceState` advertises typed member `deviceState`: an array of ASCOM
`Name`/`Value` objects. Safety evaluates current permission; Switch clones each
source cache once and derives both channel values from the same sample; Weather
reads its shared engine under one lock and monotonic time. Failed, stale, retired,
or unconfigured Switch/Weather readings are omitted independently. Empty state
returns `[]`. This collection never issues source reads or renews evidence.
TimeStamp is omitted because mixed cached samples have no single UTC measurement
time; query time would imply freshness that was not observed.

`switchAsyncContract` advertises CanAsync, StateChangeComplete, SetAsync,
SetAsyncValue and CancelAsync. These scalar channels report CanAsync=false.
StateChangeComplete and async setters return unsupported after ID validation;
CancelAsync validates the ID and succeeds because no asynchronous change can have
started. This does not claim physical completion for upstream writes.

### Local endpoints and ownership locks

`Endpoint::for_config` requires an existing absolute regular configuration file.
It hashes the canonical path and OS user identity; Windows normalizes path case.
Atomic configuration replacement preserves this identity. A separate persistent
lock file uses the OS exclusive file lock and is never unlinked on release. A
crashed process therefore releases ownership without PID-file guessing or deleting
another process's lock. Binding requires possession of this lock. Accepted streams
retain ownership if their listener is dropped; the host must additionally retain
the listener/lock until runtime shutdown has drained device work.

Windows uses the current process SID and a protected, explicit user-only DACL for
the named pipe and `%LOCALAPPDATA%/Regain/Hub` storage, resolved through the OS known
folder API. Existing storage and opened pipe handles are checked for owner/DACL;
permissive ACLs are rejected without silent repair. Reparse points are rejected
for the storage directory and lock file. Pipes reject remote clients, and clients
use identification-only impersonation rights.

Unix uses `/tmp/regain-hub-UID` with mode 0700, a mode-0600 socket and lock, and
peer-UID checks in both directions. Existing non-private directories, links,
hard-linked lock files, and non-socket endpoint files are rejected. Only the lock
owner can remove a stale socket. Listener cleanup checks the socket's device and
inode before unlinking, preserving a replacement file.

Endpoint connection retries only not-ready errors until its supplied deadline;
permission failures return immediately. A connected stream is not readiness proof:
the launcher validates the versioned hello and expected hub identity. A second
`--hub-host` invocation probes the existing owner within ten seconds and exits;
it never constructs competing sources. The first owner binds and prepares the
runtime, then serves at most 32 local clients, each with the IPC request bounds.
Clients with malformed protocols retire independently. Backpressure stops accepting
more streams while the client limit is reached.

Shutdown closes clients and drains source actors before releasing the OS lock.
Dropping a service waiter requests shutdown through an independent supervisor;
the Tokio runtime must stay alive until its cleanup completes. Cleanup failures
remain visible and are not replayed. Last-lease cleanup and later actor shutdown
share one disconnect result; a genuinely new connection starts a new cleanup
lifetime. Frontends must explicitly attach and reacquire leases after connection
loss; the client never replays commands.

Windows tests cover anonymous denial,
permissive-storage rejection, cross-process contention/crash recovery, and actual
IPC. Endpoint commit `0c8bfe7` also passed Linux x64/ARM64 and macOS Intel/ARM64 CI,
including Unix permission/link/socket-cleanup fixtures. The host integration's
portable checks are separate and must pass before this checkpoint is complete.

### Frontend attachment and Rust client

`regain-alpaca --hub-attach --hub-config ABSOLUTE_PATH [--workers DIRECTORY]`
finds or launches the shared host and prints one JSON attachment record. It
requires an existing valid configuration and accepts no HTTP, stdio, or simulation
options. Explicit simulated sources belong in the configuration. A held ownership
lock causes a bounded readiness wait, never a replacement launch. Concurrent
launch candidates arbitrate through the host's existing OS lock before creating
source actors. Readiness failure does not trigger another launch or kill an owner.

The record contains protocol, hub/host/revision identities, transport and endpoint
address. Its optional `startedProcessId` identifies a candidate, not ownership:
that process may have lost a startup race. Never kill it on frontend disconnect.
The readiness connection ends before the helper exits. Each frontend opens its
own private connection, checks OS ownership/permissions, and performs hello again.
Endpoint metadata alone does not authenticate the peer.

Windows launch uses a hidden process with handle inheritance disabled, including
capture pipes unrelated to its standard handles. Arguments preserve spaces and
Unicode without a shell. Unix uses null standard streams and a separate process
group. The host outlives launchers and must not enter a frontend's kill-on-exit
worker job. Application shutdown closes its own client, not the shared host.

`regain_hub::client::Client` verifies hello identity and negotiated limits, bounds
encoding and in-flight requests, and correlates out-of-order replies. Request
deadlines include queue/write/reply time. Partial-frame and write deadlines are
separate; idle established streams remain valid. Cancellation before dispatch
skips the command; cancellation after dispatch retains its slot until reply or
connection failure. Mutating requests handed to the writer report uncertainty
after transport loss and are never replayed. Protocol failures, unknown/duplicate
reply IDs, deadlines, and EOF make the connection terminal. Dropping the last
client closes both transport halves so stream-owned leases can drain.

Explicit reattachment negotiates new host/client identities. A closed client
cannot become connected again. Closing a client is not proof that server-side
cleanup has completed. The shared .NET client is now implemented below; actual
native providers, frontend reconnection policy, complete shared setup, and OS
resume invalidation remain required.

### Shared .NET attachment and client

`Regain.Hub.HubAttachment` and `HubClient` live in the existing shared frontend
assembly, `Regain.Rotator`, and build for .NET 8 and .NET Framework 4.8. This avoids
another executable or per-output transport. Native NINA/ASCOM providers still
need to adopt them; the client alone does not add selectable devices.

AttachAsync requires fully qualified existing paths, bounds configuration reads
at 4 MiB, and invokes `regain-alpaca --hub-attach` once. The hidden helper has a
15-second deadline and 16 KiB limits for stdout/stderr. It is not attached to a
device worker job. Cancellation/failure stops only that helper, never its child
or the reported candidate PID. Neither raw stderr nor request bytes appear in
exported exceptions. The attachment record must match the selected configuration
identity and use the expected local pipe namespace. It is a snapshot, not proof
that a configuration revision remains current.

ConnectAsync opens a non-inheritable, asynchronous named pipe with identification
impersonation rights. Before hello, it verifies the opened handle's owner and
protected DACL: exactly one ordinary allow ACE for the current user. It rejects
remote names, unrelated pipe namespaces, permissive descriptors and changed host
identities. This mirrors the Rust endpoint check using
[GetSecurityInfo](https://learn.microsoft.com/en-us/windows/win32/api/aclapi/nf-aclapi-getsecurityinfo)
on the actual handle. The implementation uses
[identification rather than impersonation](https://learn.microsoft.com/en-us/windows/win32/secauthz/impersonation-levels).

The client negotiates at most eight requests and 1 MiB frames, correlates
out-of-order replies, distinguishes null from absent results, rejects duplicate
JSON keys/unknown envelope fields or reply IDs, and retains structured field
errors and retry delays. Outgoing token/byte limits precede dispatch; only known,
advertised operations are callable. Unknown future operations are not assumed
read-only. Idle streams remain valid; partial frames and writes have five-second
default deadlines, while the 35-second request deadline includes queue/write/reply.

Queued cancellation returns capacity without sending. Dispatched cancellation
ends the caller's wait but retains its request slot/deadline until reply or
terminal loss. A dispatched mutation reports uncertainty after transport failure;
it is never replayed. Dispose closes this client's stream, not the host. Pump
tasks retain the connection state rather than the public client, so an abandoned
client can be finalized and release its leases. Closed means terminal transport,
not completed server-side hardware cleanup. Reattach explicitly and read current
configuration/status before reconciling a lost mutation.

Tests exercise faults, actual permissive-pipe rejection before hello, two managed
clients sharing one Rust host, independent leases, abandoned-client cleanup, and
real net48 x86/x64 processes through the public API. They use simulation only.
Native device providers, shared native setup, reconnect/resume and conformance
remain separate acceptance gates.

### Initial Alpaca output adapter

Ordinary `regain-alpaca --hub-config ABSOLUTE_PATH` attaches or launches the local
host and publishes its Switch, SafetyMonitor, and ObservingConditions alongside
existing devices. Management discovery preserves output UUIDs and configured
device numbers, including gaps. Simulation labels are explicit. No hub source
actors or safety policies run inside the HTTP frontend.

The catalog uses one private connection. Up to 24 distinct connected Alpaca
ClientIDs each have a separate private session, leaving host capacity for native
frontends and setup. These IDs are correlation/lease identities, not authentication.
Disconnecting the last output retires that session and returns capacity. There is
no implicit idle eviction of a connected device. Changes for one client are
admitted before spawning a supervised task; overlapping changes return busy.
Accepted changes survive loss of their HTTP waiter. Failed connection changes
revoke that client's entire private session to release uncertain leases. An
asynchronous failure retains its error in the bounded client slot until explicit
Connect/Disconnect reconciliation; polling Connecting cannot mistake it for
successful completion. A failed synchronous call returns its error and retires
the slot. Explicit reconnect replaces a failed session; disconnect clears it.

`Connected` reports a lease on the virtual output, not the health of every source.
Safety remains false until its host policy permits operation; missing or failed
switch/weather readings return errors. Each getter requests current host state;
the frontend never caches safe permission. Weather IPC retains provenance and age,
while its Alpaca measurement property returns the scalar value. SensorDescription
uses a negotiated `weatherSensorDescription` capability so an older host is not
sent an unknown typed request.

With the three negotiated capabilities above, the adapter advertises Switch 3,
SafetyMonitor 3 and ObservingConditions 2. Connect/Disconnect return after bounded
admission, and Connecting includes the HTTP adapter's private-pipe initialization
as well as the host operation. Legacy Connected remains synchronous. Older hosts
retain Switch 2 and SafetyMonitor/ObservingConditions 1; unsupported capabilities
are checked before sending new typed requests. DriverVersion uses major.minor.
DeviceState uses canonical operational names, including stable GetSwitchN and
GetSwitchValueN slot numbers and StarFWHM. It omits unavailable entries as described
above. Complete protocol/error conformance and conformance-tool runs remain
required before final acceptance. The implementation was reviewed against the
[ASCOM read-all guidance](https://ascom-standards.org/newdocs/readall-faq.html),
[timestamp guidance](https://ascom-standards.org/newdocs/timestamp-faq.html), and
[Switch](https://ascom-standards.org/newdocs/switch.html),
[SafetyMonitor](https://ascom-standards.org/newdocs/safetymonitor.html) and
[ObservingConditions](https://ascom-standards.org/newdocs/observingconditions.html)
contracts.
Renaming channels uses configuration; SetSwitchName and arbitrary actions/commands
are unsupported. The initial shared setup UI is described below.

Host loss fails requests and does not reconnect or replay mutations. Reattach the
HTTP frontend explicitly to obtain a new catalog/session set. Server shutdown or
process death closes its private streams; the shared host and other clients remain
alive. Restart/configuration-edit UI and OS resume handling remain pending.

### Shared web configuration editor

`/setup/hub` and the three scalar device setup URLs serve the same schema-driven
editor. The main equipment page links to it. Rust describes fields, tagged choices,
defaults, bounds, units, source/output references, immutable identities, and
capability-gated enum choices. JavaScript and .NET readers consume that metadata;
the editor does not redefine device or safety policy parameters. Source and output
UUIDs are generated locally with cryptographic randomness, including on LAN HTTP.
Saved numbers/IDs stay read-only; deleting an item preserves the host's tombstones.

The editor keeps an unsaved draft separate from the loaded revision. Review uses
host validation and a metadata-redacted configuration preview. Editing invalidates
the reviewed candidate. Apply sends that candidate and its expected revision;
connected clients or stale edits are rejected by the host. Lost/uncertain apply
responses require reloading saved configuration/status before another attempt.
Applied-but-blocked state disables editing and remains distinct from a failed save.

The POST-only setup API accepts bounded JSON with same-origin checks and no CORS
grant. It allows description/configuration/status, validation/apply, source
inspection, and explicit simulation updates; it cannot issue output connection or
device commands. Responses are no-store and retain field errors/uncertainty. This
is the existing Alpaca server's trusted-network setup surface, not remote-user
authentication. Credential creation/deletion is not exposed here yet.

Source Status reads the actor snapshot. Inspect is explicit because it takes a
temporary connection lease; page start/size labels, bounds, and defaults come from
the shared inspection descriptor. Inspection does not manufacture safety evidence.
New configuration initialization, credential UI, simulation controls, richer
safety/health diagnostics, native setup adoption, and reconnect/resume UI remain
required refinements. Large-file editing also remains bounded by the 1 MiB IPC
frame limit even though file storage has a separate 4 MiB ceiling.

## Identities and configuration

Persist schema version, configuration revision UUID, hub instance UUID, source
UUIDs, output UUIDs, device numbers, and switch channel UUIDs/numbers. Numbers are
explicit and unique within a device class/output, never derived from list order.
Deleted numbers are not reassigned to a new identity during config updates.
Source references use UUIDs. A virtual source reference forms a directed graph;
validate it for dangling edges, type compatibility, and cycles before applying.

Source variants are native worker, remote Alpaca, Windows COM, and another virtual
output. Simulation is explicit and visibly labelled. Normalize remote URLs for
identity comparison; reject credentials, query strings, and fragments in endpoint
URLs. Credentials are references to local protected storage. Capability discovery
and live observations are separate from configuration.

Apply configuration by validating a candidate, checking expected revision and
active-operation constraints, replacing the file, then draining the previous
runtime before publishing a usable replacement. Validation, construction, staging,
or replacement failures leave the old running configuration intact. Failures
after replacement have distinct outcomes below; they must not claim rollback.
Version 1 is the first hub schema; missing or future schema versions fail with an
actionable error rather than guessed defaults. Existing camera profile migration
is a distinct, tested adapter; it does not reinterpret a hub document as a profile.

The current implementation rebuilds the whole runtime, so all its outputs are
affected and must be disconnected before apply, including pending connects and
commands retaining connection guards. The check and connection reservations share
a short lifecycle lock; staging and device I/O hold no service-state mutex.
Any applied change starts a new generation with no cached
safe permission. Cosmetic-only live edits can be added later with proof that they
cannot change source identity or policy. This intentionally differs from Field
Kit's bounded retention across edits.

### Applying through IPC

Send `{"op":"applyConfig","expectedRevision":"UUID","candidate":{...}}` in
the command envelope. Both expectedRevision and the candidate revision must match
the current snapshot. Candidate identity history cannot be edited. Preparation
registers new identities, assigns the next revision, and writes and flushes a temporary
file beside the destination. The next runtime is constructed without device I/O.
Commit rechecks the revision and originating store before atomic replacement.
The saved file is flushed again, and Unix also flushes its containing directory.
Configuration reads are bounded to 4 MiB; IPC retains its smaller frame limit.

The normal result contains `applied: true`, `ready: true`, a new
`configurationRevision`, and empty `cleanupErrors`. Existing local streams keep
their client identity and bind to the new runtime on their next device request.
The hostInstance stays fixed until process restart. Old runtime clients are closed
and cannot reopen sources.

If old-source cleanup is uncertain after commit, the result is `applied: true,
ready: false` with source cleanup errors. The saved revision is retained, but
device admission is blocked until the host is restarted after equipment checks.
The host never activates replacement sources over unfinished old ownership.
`hostStatus` reports ready/applying/blocked/stopped plus the persisted revision and
cleanup errors. `getConfig` and `hostStatus` remain readable during apply or a
blocked state. An unexpected update-task failure also blocks admission.

A final filesystem flush failure after replacement adds `persistenceWarning` to
the result/status; memory and disk still reflect the new revision. This is not a
pre-commit I/O error and must not invite retry with the old revision. Flushes use
the platform/filesystem's guarantees; no power-loss test is claimed here.

Once accepted, apply runs under its own supervisor through commit and cleanup.
RPC timeout returns uncertain; EOF or cancellation does not roll back or replay
the transaction. Reload getConfig/hostStatus to resolve the outcome. Concurrent
edits return busy or revisionConflict. Host shutdown waits for accepted apply
before releasing endpoint ownership.

## Safety observations and time

Every observation carries configuration revision, source generation, monotonically
increasing sequence number, request start, response receipt, and a typed outcome.
An attempt failure is distinct from an exhausted poll cycle. Invalid timestamps,
late generations, and replayed sequences cannot establish permission. Runtime
timestamps are monotonic durations relative to a host clock and are never saved.
Explicit resume/clock discontinuity invalidates generations and evidence.

Sampling belongs to a source; policy belongs to each output membership. All unsafe
readings and failed attempts clear recovery progress immediately. Confirmations
are counted no faster than that membership's configured cadence, measured from
request starts. Getter calls and faster consumers never accelerate confirmations.
Source polls honor the fastest required cadence; a consumer can use a slower
confirmation cadence without opening another connection.

The engine owns a periodic expiry task independent of source polling. Status
reads also reevaluate expiry; neither network latency nor retry backoff extends
the maximum safe age. Recovery requires both count and hold, and only a new
eligible safe observation can establish permission. Aggregation is AND with no
additional hidden debounce. See the plan for defaults and failure behavior.

## Switch and weather controller semantics

Switch numbers are bounded stable slots, not positions in the active channel list.
MaxSwitch includes retired slots; a removed slot has a placeholder name, is
read-only, and returns unavailable for its value. Numeric writes must be inside
the configured range, are rounded to the nearest configured step as ASCOM
specifies, and must also fit the source's live range/step and CanWrite response.
Property gauges cannot be made writable. Boolean false maps to the channel
minimum; true maps to its maximum. A source generation check at dispatch prevents
using capability evidence from a retired connection. Every command has a distinct
control owner, even when two calls originate from one frontend session.

Weather freshness includes upstream TimeSinceLastUpdate for ObservingConditions
inputs, measured conservatively from the local batch request start. Other scalar
sources report the age of Regain's read; this does not imply an unavailable
upstream sensor timestamp. Each metric chooses its first fresh valid source.
Sensor-specific failures are independent; a transport failure still affects the
source. Last-update diagnostics retain elapsed age after expiry; unavailable data
is never silently reported as a fresh value.

Known property units cannot be overridden. For channels or unknown properties,
the readout must declare a canonical unit before use as weather. Temperature is
°C, pressure hPa, rain rate mm/h, sky brightness lux, sky quality mag/arcsec²,
star FWHM arcsec, direction deg, wind speeds m/s, and cloud/humidity percent.
No implicit conversions or dew-point derivation occur. Humidity/dew point are a
pair; direction requires wind speed so calm returns zero. WindGust currently
requires an ObservingConditions windgust source, preserving its three-second
peak over two minutes instead of pretending an arbitrary gauge has that meaning.

ASCOM has one AveragePeriod (hours) per output. Configured non-gust measurement
intervals must agree; runtime changes are shared by that output's clients and
restart restores its configured value. Zero is instantaneous. Nonzero averages
are time-weighted over a bounded history, with a circular mean for direction and
an explicit unavailable result for an undefined circular mean. Early averages use
the available observations; getters never insert duplicate samples. Fallback,
generation changes, and observation gaps clear history. WindGust passes through
its source statistic without a second averaging step.

Each Alpaca sample step makes at most one HTTP request. Age and value requests
share a conservative age anchor but have separate deadlines. Commands can run
between steps. Snapshots retain per-key observation time and sequence; another
property's response cannot rejuvenate a measurement or grow its average. Scalar
caches allow at most 1024 keys (including historical sequence counters) and 1 MiB
of text. Failed measurements remove only their own cached value.

Refresh is a short acquisition trigger, not a wait for new sensor values. Remote
ObservingConditions sources receive PUT Refresh; other scalar sources schedule a
new local poll. Source requests run concurrently across a weather output, while
each actor serializes its own I/O. Retry-After prevents a refresh from bypassing
an upstream delay. Refresh does not rewrite sensor ages or replay on failure.
Clients observe TimeSinceLastUpdate to determine when measurements change.

These are controller contracts with local latency/retry tests, not evidence of
completed frontend interface conformance or hardware acceptance.

## Alpaca connection negotiation

Source construction first validates the complete configuration and derives the
union of readouts needed by its outputs. Duplicate readouts share one sample;
ObservingConditions properties retain their sensor-age request even when used
as Switch gauges. The 1024-sample limit applies to the union, and construction
stops as soon as that limit is exceeded. SafetyMonitor plans contain only one
strict IsSafe observation per attempt. Enabled safety memberships can shorten
the effective source interval to their fastest confirmation interval; persisted
settings and each membership's own confirmation policy remain unchanged.

The factory prepares native/Alpaca adapters and resolves credential references
before spawning any actors. It does not open devices or make network requests.
Each source gets one actor regardless of the number of outputs. A host without
a protected credential provider rejects credential-bearing source configuration.
The executable supplies the user-scoped credential provider described below.
The factory also prepares Windows scalar COM imports, explicit simulation and
local virtual-output sources. Native-camera and broader COM classes remain later
implementation work; no unavailable backend falls back to another transport.

Read InterfaceVersion before opening a source. Modern connection methods start
at Camera/Focuser/Rotator V4, Switch/SafetyMonitor/FilterWheel V3, and
ObservingConditions/CoverCalibrator V2. Older interfaces use Connected. Missing
InterfaceVersion (NotImplemented or HTTP 404) permits legacy compatibility;
malformed values, authentication failures, and other upstream errors do not.
A claimed modern interface whose Connect fails does not silently fall back.

Externally managed sources only read connection state. Managed modern sources
call Connect with their stable ClientID even when shared hardware is already
connected, poll Connecting until false, then verify Connected. Legacy sources
already connected are borrowed and never disconnected by the hub. Only an
acknowledged opening grants ownership for cleanup. A cancelled or uncertain
Connect/Disconnect is not replayed, including on reset or shutdown.

Each handshake step is one bounded request. `connectionTimeoutSeconds` defaults
to 30 seconds (1–300), independently of `requestTimeoutSeconds`; asynchronous
waiting cannot continue indefinitely. Pending steps do not emit poll failures
or safe observations. Reopening after an acknowledged asynchronous Disconnect
waits for its Connecting state to finish before claiming another connection.
Snapshots report negotiated version/method, ownership, and uncertainty. These
diagnostics do not imply broader device capabilities have been discovered.

Interface references: [ASCOM common behavior through SafetyMonitor V3](https://ascom-standards.org/newdocs/safetymonitor.html),
[Camera V4](https://ascom-standards.org/newdocs/camera.html),
and [ObservingConditions V2](https://ascom-standards.org/newdocs/observingconditions.html).

## Credential storage and rotation

The private host advertises `createCredential`, `credentialStatus`, and
`deleteCredential` only when it has a storage provider. `describeConfig` supplies
`credentialStorage` with the protection method and the shared authorization input
key, label, description, length limit, and write-only/sensitive flags. An absent
provider reports null and refuses credential-bearing source construction.

`createCredential` takes `authorization`, the complete HTTP Authorization header
value (for example a Bearer token). Values must be nonempty ASCII without control
characters, at most 8192 bytes. The reply contains a new opaque `reference`,
`present`, and `protection`; there is no operation to retrieve the value.
`credentialStatus` takes `reference` and returns the same status shape. Missing
records report `present:false`; invalid storage/ciphertext reports an error.
Configuration and diagnostic responses contain no credential values. Raw IPC
frame buffers and owned secret serialization buffers are cleared on drop; this
is not a guarantee that every OS, JSON parser, or HTTP-library copy is erased.

| Platform | Reported protection | Storage |
| --- | --- | --- |
| Windows | `windowsDpapiUser` | User DPAPI encryption plus explicit user-only protected ACLs; LocalAppData/Regain/Hub/Credentials |
| Linux/other Unix | `userFilePermissions` | Plaintext in mode 0600 files under a mode 0700 scope directory; absolute XDG_DATA_HOME or HOME/.local/share, then regain/hub-credentials |
| macOS | `userFilePermissions` | Same private plaintext format; absolute XDG_DATA_HOME or HOME/Library/Application Support, then regain/hub-credentials |

Windows uses DPAPI with UI forbidden and without machine-wide protection. There
is no plaintext fallback. See [CryptProtectData](https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata).
Unix needs no desktop keyring, which permits headless service accounts. Frontends
must identify private plaintext storage accurately. A host with no resolvable user
storage location can still run sources that do not use credentials.

Each scope is keyed by canonical configuration path and OS user; copied/moved
configurations require newly created references. File names hash the scope and
reference. Records verify both values and their format version; Windows also
binds them into DPAPI entropy. Reads are bounded to 64 KiB and reject inappropriate
permissions, nonregular files, and final symlinks/reparse points. Unix also rejects
hard links. Never export these records with configuration or support bundles.

References are immutable. Rotate by creating a new reference, applying it through
revision-checked configuration, then deleting the now-unused old reference.
`deleteCredential` refuses any reference still present in configuration, even in
an unused source. It returns `removed`, plus a persistence warning if deletion
succeeded but final directory flushing failed. Deleting a missing record is a
successful no-op. It does not promise forensic erasure of filesystem backups.

Creation/deletion and configuration apply share the update gate. Accepted storage
mutations run off the async executor and retain that gate after RPC cancellation;
host shutdown waits for completion. A timed-out mutation has an uncertain result
and is not automatically replayed. A failed/lost creation reply can leave a private
unreferenced record; it cannot overwrite an existing reference or rotate a live
source. Credential resolution during runtime preparation also runs off the async
executor. Storage tests and private IPC tests do not establish frontend UI support.

## Native source adapter checkpoint

### Setup capability inspection

`inspectSource` takes a configured source UUID, `start`, and `limit`. It acquires
a temporary source lease through the same actor used by outputs. It may open
and close a connection under the configured ownership policy, but sends no
equipment control commands. `describeConfig.capabilityInspection` supplies shared
parameter keys, labels, limits, defaults, and the 20-second overall deadline.
Construction and ordinary config validation still perform no device probing.

The initial inspection supports Alpaca Switch, SafetyMonitor, and
ObservingConditions sources, plus the seven native accessory families below.
It reports `purpose:setupOnly`, source/configuration/generation identities,
negotiated connection information, and host-clock start/completion times. Native
simulation is explicit. Other network device classes remain unsupported here
until their broader proxy/capability contracts are implemented in milestone 4.

- Switch pages contain at most eight real upstream channel IDs, names,
  descriptions, CanWrite, bounds, steps, and a next-start index. MaxSwitch must be
  a valid nonnegative ASCOM Int16 count. Range validation reuses the exact grid
  validator used by writes. Unknown/invalid bounds cannot suggest a writable
  mapping. No unit is inferred from a Switch label or description.
- Weather inspection reports all thirteen standard measurements, canonical units,
  SensorDescription, TimeSinceLastUpdate, and a numeric reading as separate
  observations. Failure of one field does not invent support or erase another.
- Safety inspection accepts only a Boolean IsSafe. This diagnostic read is not
  a policy observation or permission to operate equipment; only the safety engine
  produces the combined decision and advances confirmation counters.
- Native inspection reuses each adapter's property map. Numeric readings can be
  offered as scalar mappings; Boolean accessory properties are currently marked
  ineligible because the scalar controllers do not consume them. An absent optional
  temperature sensor is unavailable, not a fabricated reading or capability.

Probe states are `observed`, `unsupported`, and `unavailable`. Only a definitive
NotImplemented response produces unsupported; authentication/transport failures,
invalid values, and temporary sensor failures remain unavailable. Strings are
bounded to 1024 Unicode characters without controls; numbers must be finite.
Metadata strings are untrusted device text and must be escaped by UI renderers.

Requests are sequential through the source actor, so polling and other commands
can interleave. Each read is fenced against the initial generation at dispatch,
and the scan rejects any detected connection change instead of combining epochs.
Pages carry their own generation; they do not promise an atomic device snapshot.
Writes continue to revalidate live permissions and bounds. Inspection never
updates cached telemetry, sensor ages, or safety evidence from its getter results.

An active inspection participates in configuration quiescence. Cancellation or
deadline expiration drops its temporary lease; an already-dispatched read may
finish within the source's request deadline before queued cleanup runs. Runtime
replacement still drains the old registry. A Retry-After response stops the scan
without replay; IPC errors expose `retryAfterSeconds` for the requesting frontend.
No inferred permission or default value substitutes for an unsuccessful probe.

References: [Switch interface](https://ascom-standards.org/newdocs/switch.html) and
[ObservingConditions interface](https://ascom-standards.org/newdocs/observingconditions.html).

### Local output sources

A source with `{"kind":"virtual","output":"OUTPUT_UUID"}` reads a configured
output in this same hub runtime. Switch, SafetyMonitor, and ObservingConditions
outputs can feed another output without a loopback HTTP listener or a second
device connection. The graph validator rejects direct and indirect cycles before
actors start. Standalone source construction rejects virtual sources unless it is
bound to a shared output runtime.

Each virtual source holds an internal client lease while connected. Direct and
nested clients share the underlying controllers and actors, so closing one path
does not disconnect another. Source control, actual write permissions, value
grids, generation fences, and uncertain-write handling still apply at each layer.
An ambiguous nested write is not replayed. Other clients holding the leaf session
continue to see its uncertain-write latch. Shutdown closes internal clients and
drains all actors; pending and retained internal operations count toward apply
quiescence. A weak runtime binding avoids retaining an idle graph indefinitely.

Scalar sampling forwards the selected reading's age, including through multiple
layers. Missing or stale measurements stay errors. Weather Refresh reaches the
underlying sources; configured averaging applies at each weather output, so
chaining two averaging outputs deliberately adds two processing stages.

Safety carries the oldest contributing safe request timestamp through local
composition. Repeated polls of an unchanged inner result cannot add confirmations
or extend the outer maximum age. An inner output in communication grace supplies
failed-read evidence, preserving the outer grace/expiry policy without allowing
aggregate recovery from a cached success. A newly constructed or reconnected policy ignores
safe evidence predating its freshness fence and waits for a new observation. Each
output keeps its configured confirmation and unsafe-transition policies; nested
policies can therefore add response latency. Getters and capability probes cannot
create safe evidence.

Simulation marking propagates through the dependency graph, including mixed
outputs. Setup inspection and scalar IPC use the same typed output operation
handlers as virtual sources. This implementation covers local composition of the
first three classes. COM imports and NINA/HTTP publication now use these same
controllers; camera proxies and native ASCOM output publication remain pending.

### Explicit scalar simulation

`simulated` sources currently implement Switch, SafetyMonitor, and
ObservingConditions through the same actors, leases, polling, and output policies
as native/network sources. Other classes remain unsupported until their proxy
contracts are implemented. Simulation is never a fallback for a failed device.
Source status, output listings, and setup inspection mark simulated data. An
output with any simulated dependency is marked simulated, including mixed outputs.

Private IPC `updateSimulation` takes a source UUID and a typed `update` object.
`describeConfig.simulationControl` supplies its JSON Schema, field descriptions,
class-specific controls, and supported faults. Updates are atomic, require the
source control lease, and participate in configuration quiescence. They invalidate
cached samples and schedule a new poll; they do not directly grant safety
permission. Ordinary device commands retain the production permission/range checks.

Switch channels are a writable relay (0, range 0–1), a writable level (1, range
0–100), and a read-only temperature sensor (2, range −40–80 °C). Test controls can
inject sensor readings, but ordinary switch writes cannot change read-only sensors.
Weather supports the thirteen standard metrics; a null injected reading removes
that sensor. `sampleAgeSeconds` injects stale evidence, while Refresh resets its
age to zero. Safety starts false in every new runtime and requires normal policy
confirmation after a true raw reading is injected.

Faults include read failure, timeout, invalid safety values, and switch writes
that change state but return an uncertain result. Clearing a fault does not clear
the actor's uncertain-write latch; all source leases must disconnect first.
Injected state lives only in the runtime: connection reset preserves the test
scenario, while configuration replacement or process restart restores defaults.
There is no persisted "start safe" option.

Use [simulated-observatory.json](../crates/regain-hub/examples/simulated-observatory.json)
with `regain-alpaca --hub-host --hub-config ABSOLUTE_PATH` to run all three classes
without attached equipment. This currently exposes private IPC; HTTP publication
and native NINA outputs also use the same host. Ordinary HTTP mode publishes the
configured classes; `--hub-host` opens no HTTP listener. Complete native setup
editing and ASCOM output adoption remain pending.

### Native scalar adapters

Native accessory sources launch the existing `regain-device VENDOR DEVICE serve
--serial ID` worker through `regain-core::accessory`. Construction does no I/O;
connection verifies the returned serial before exposing the source. The host's
explicit simulation setting is retained in identity diagnostics for every vendor
and is never enabled as a fallback after a connection failure. Requested source
deadlines apply to worker I/O as well as the enclosing source actor.

| Source | Initial scalar readings | Initial commands |
| --- | --- | --- |
| EAF / FocusCube3 | Position, movement, temperature, travel limits | Move, halt |
| ETA | Position, movement, travel limits | Move; no hardware halt capability |
| EFW | Position and stable slot count during calibration | Select position, calibrate |
| CAA / Falcon | Logical/mechanical/target position, movement; CAA temperature, Falcon reverse | Relative/absolute/mechanical move, halt |
| OFP2 | Brightness, maximum brightness, cover and calibrator state | Cover open/close/halt, calibrator on/off |

Properties become read-only Switch gauges or Weather measurements where their
units/type permit. Sources retain independent connection/control leases; native
commands pass through the same source actor and uncertainty latch as Alpaca.
An `ok:false` worker reply may follow a dispatched USB command, so writes report
uncertainty rather than promising no motion occurred. Only locally validated
bad parameters/unsupported members are rejected before worker dispatch.

This table describes the current library adapter, not a completed proxy driver.
Camera sources still require regain-core Session and its acquisition/recovery
contract. Rotator sync/reference persistence, complete capabilities, custom
actions/settings, and frontend interface mappings remain milestone 4 work.
Existing CAA/EAF/EFW workers may enumerate/open matching HID candidates to locate
their serial; this adapter reuses that selection behavior and does not claim
target-only USB opening. Serial workers filter their selected identity first.

## Windows COM and interface baseline

Use one `Regain.Hub.ASCOM` project/executable with import-worker and output-server
modes, built x64 and x86 where import driver registration requires it. Each COM
source owns an isolated process and STA with a message pump. Marshal all driver
calls to that STA; enforce deadlines in the Rust parent, terminating a hung
worker and invalidating its generation. Do not abort a CLR thread or reuse a
timed-out COM object. Preserve source errors after sanitization. Setup is an
explicit interactive operation, never run while polling.

Pin the existing `ASCOM.DeviceInterfaces` 7.1.2 and `NINA.Plugin` 3.2.0.9001
packages. The installed 7.1.2 declarations provide Camera V4, Switch V3,
SafetyMonitor V3, ObservingConditions V2, Focuser V4, Rotator V4, FilterWheel V3,
and CoverCalibrator V2. Imports negotiate capabilities with older interfaces;
unsupported operations stay unsupported. Verify each implemented output against
its actual interface and conformance tool before declaring support.

### Import-worker protocol checkpoint

The private executable accepts `--import --prog-id PROGID --device-type TYPE
--connection-policy POLICY --bitness x86|x64`. TYPE currently admits `switch`,
`safetymonitor`, and `observingconditions`; POLICY is `managed` or
`externallyManaged`. Bitness must match the worker before activation. The helper
is built under `hub-ascom/x86|x64/Regain.Hub.ASCOM.exe` alongside the Rust workers.
The Rust source factory adopts these workers in the development branch. Windows
NINA/ASCOM payloads include both private architectures and dependencies; these
are import helpers, not registered native ASCOM hub outputs. Schema capability
choices advertise only installed helper architectures and the three scalar
classes. Other platforms offer exported Alpaca sources instead.

Requests are UTF-8 newline JSON, fewer than 4096 bytes before the newline, with
`protocol: 1`, a positive strictly increasing signed-64-bit `id`, and `operation`.
Operations are `connectStep`, `disconnectStep`, `read`, `write`, and `refresh`.
Only read/write accept `member` and a parameter object. Unknown fields/operations,
duplicate fields, invalid UTF-8/JSON, depth over 32, oversized/truncated frames,
and replayed request IDs terminate the stream. Do not try to resynchronize it.

An acknowledged response, including a sanitized vendor error, is
`{ "ok": true, "result": { "protocol": 1, "id": ID, "value": VALUE,
"error": null, "connection": INFO } }`. Error is instead `{ "kind": KIND,
"code": HRESULT_OR_NULL }`; no vendor message or stack is returned. KIND is
`unsupported`, `invalidValue`, `disconnected`, `unavailable`, `permanent`,
`transient`, or `uncertain`. INFO contains `deviceType`, nullable
`interfaceVersion`, `method` (`legacy`/`async`), `ownsConnection`, `uncertain`
(connection ambiguity), and `ready`. Replies are bounded below 1 MiB. The parent
must validate reply identity/types, retire corrupt streams and uncertain
generations, and map errors without replaying dispatched mutations.

Connect steps activate, discover version, inspect connection, claim an owned
connection if needed, wait for modern Connecting, and verify Connected. A missing
InterfaceVersion reported as NotImplemented selects legacy connection; a modern
Connect rejection never falls back to a setter. Externally managed imports never
change connections. Legacy managed imports borrow an already connected driver;
modern managed imports call Connect to own their private driver/client connection
even when shared hardware reports Connected. Each ownership/cleanup change is
consumed before invocation. Failed verification can still release acknowledged
ownership on graceful shutdown; uncertain changes cannot be repeated. A generic
failed switch write latches further writes until the worker is retired; reads
remain possible for diagnosis.

Whitelist common metadata and typed members of these three interfaces. Convert
weather sensor keys to canonical ASCOM spelling, retain upstream ages, reject
nonfinite readings and non-Boolean IsSafe. The worker does not call Dispose on
imported drivers: implementations may disconnect shared hardware there. Release
the RCW on its STA; graceful EOF attempts only acknowledged owned cleanup.
Terminate a hung Regain worker through parent ownership/deadlines, never a shared
vendor COM server. Forced worker termination cannot prove upstream Disconnect
completed. The parent attaches its kill-on-close job before the first activation
request, with silent breakaway for vendor descendants: ownership covers the
private Regain worker, never a shared vendor server/helper. The existing bounded
accessory transport now supports typed inner replies directly from frame bytes;
duplicate fields cannot disappear through an intermediate JSON map. Invalid
identity, types, framing or contradictory ownership retires that worker.

Each RPC has a configured deadline; the complete connection and disconnect
handshakes have independent total bounds. Cancellation retires a pending worker.
The actor fences lost generations and retains write uncertainty; an ambiguous
Connect/Disconnect cannot be repeated in a replacement worker. A read stall may
recover through a new private worker without claiming that forced termination
confirmed upstream cleanup. Shared scalar sampling preserves same-key retry
budgets, independent weather failures and conservative source ages. Safety
permission expires in the host independently of the blocked COM call.

Registered fixture tests cover both architectures through the actual Rust
factory, source actors and output policies, including mixed native simulation,
loopback Alpaca and COM inputs. These establish transport/policy behavior, not
interactive NINA, installed vendor-driver, conformance or real-device acceptance.
Release signing includes both import executables, but remains unverified until
a release job exercises the updated development payload. Windows CI for the
worker foundation and adapter failed first activation with HRESULT 0x80070002.
Fixture-only probes show successful direct managed loading and failed COM
activation in both elevated runner processes. Elevated COM does not load
per-user classes ([Microsoft guidance](https://learn.microsoft.com/en-us/windows/win32/com/the-com-elevation-moniker)).
Local tests retain private HKCU registration. The script explicitly selects HKLM
only on an elevated disposable GitHub Windows runner; Python verifies that
context, checks both hives for collisions and removes only the private keys it
created. Updated runner validation remains required, without a production bypass.

Field Kit reference commit `8be3d38f0b04fa78d7ae36b460ed10656f259d0f` is
Apache-2.0. Its endpoint state, aggregate, service, and integration tests supply
behavioral cases; Regain adds cadence, generation, configuration, and multi-output
ownership tests. Record attribution in the shared crate and third-party notices.

## Native frontend bindings and sessions

`Regain.Rotator` holds the shared .NET 8/net48 attachment client, output selector
and selection store under `Regain.Hub`. The native NINA providers use those classes
without an HTTP publisher or ASCOM output. ASCOM adoption remains milestone 4 work.

The frontend selection file is separate from host configuration. Its format is
`schemaVersion: 1`, a nonempty saved `revision` UUID, and a `bindings` array.
Each binding contains `configPath`, `instanceId`, `outputId`, `deviceType`, `label`
and `simulated`. Configuration paths are fully qualified; labels/markers are
presentation snapshots refreshed from the live catalog at Connect. No source
parameters, safety policy, secrets or live permission are copied into bindings.
The computed NINA Id combines instance/output UUIDs and is not serialized.

The store accepts at most 64 bindings and 512 KiB of strict JSON, rejects duplicate
keys/identities and unknown members, and serializes saves under a persistent OS
lock file. Save checks the exact expected revision and writes a fresh revision by
flushed atomic replacement. An unreadable file is not treated as an empty file
and native setup cannot overwrite it. Native ASCOM registration management remains pending.

Saved-choice removal uses the same lock and revision-checked atomic persistence
as selection save. It identifies the instance/output pair, preserves other
entries and rejects stale revisions, unreadable storage or absent identities
without writing. An empty saved list remains valid. The shared themed manager
shows configuration path and immutable identities; it acquires no host or
equipment lease. Removal affects later chooser enumeration, not the host's output
configuration or connected clients' private selections. A failed/uncertain save
requires explicit reload; neither removal nor restoration is automatically replayed.

Connect copies the selection, checks its instance before launching the helper,
authenticates the live host, and matches the output UUID/class exactly. Metadata
initialization is bounded by the native device's 45-second connection deadline.
The same session also supports verified attachment without acquiring equipment;
native ASCOM uses this to delegate connection changes and completion to the host.
Each device has its own private client lease and connection epoch. Read requests
normally have a three-second caller deadline; capability/command waits allow up
to 35 seconds, surrounding the host's independent operation bound. These are
frontend transport waits, not duplicate source retry or safety policy settings.

Cancellation/disconnect during initialization cannot publish a ready device.
Requests and Switch objects from retired epochs cannot operate a reconnected
session. Remote/local-admission rejections preserve a healthy pipe; transport or
caller deadline failure retires it. Getters never launch or replay. Explicit
Connect is required after terminal transport loss, and closing the frontend never
terminates the shared host. Ending a caller wait does not promise physical rollback.

NINA Safety queries shared permission on every getter and returns false on local
failure. Weather returns NaN for each unavailable/stale metric. Switch objects
retain host slot numbers and retired gaps. Only confirmed writable capabilities
produce IWritableSwitch; unavailable capabilities remain read-only until explicit
reconnect. Writable targets use the host's exposed grid; the host checks source
permission/grid on every command. Failed readback throws because NINA's NaN
comparison would otherwise report successful completion. Poll/write versions
prevent a pre-write response from restoring invalidated cached state.

## Shared native configuration editor

`HubConfigurationDraft`, `HubEditorSession` and the themed WPF window live in the
same .NET 8/net48 frontend assembly. NINA's output selector opens this editor
through private IPC; native ASCOM adoption is still required. The editor client
has no output lease and never disconnects another frontend to make Apply succeed.

Draft fields, tagged transport/device variants and described scalar choices come
from `describeConfig`. Generated IDs are immutable; saved device/channel numbers
stay fixed. The public scalar setter cannot replace a whole record or collection
and thereby bypass those protections. Structural operations create new IDs or
remove explicitly selected items. Hidden store metadata survives in the candidate;
ordinary preview omits it and fields marked sensitive or export-omit. The host's
identity ledger and cross-field validator remain authoritative.

Review validates a cloned candidate without connecting sources. Apply requires
that exact reviewed candidate, draft version and saved revision. An in-flight
Apply is sent once. A successful reply still requires reconciliation with saved
configuration and host status; a lost reply or revision conflict cannot be
cleared by reviewing the same stale draft. Reload may attach a fresh private
client, but never retries Apply. The editor shows both revisions if another
editor commits between this Apply and the following reload.

Closing the editor cancels its waits and closes only its private client. Disposed
state is terminal even if a late response arrives; token-source disposal waits
for active operations/cancellation callbacks. Scalar parse errors retain the
invalid text across collapsed sections and block structural changes or review.
Collection controls load lazily in 32-item pages. Cached source health is separate
from live inspection, which remains a native setup refinement.

## Native ASCOM output increment

The existing `Regain.Hub.ASCOM.exe` now has import and export modes. Bound exports
implement Switch 3/2, SafetyMonitor 3/legacy and ObservingConditions 2/1, sharing
the native IPC client and themed configuration editor. Construction, catalogue
loading and metadata do not attach to the host or acquire equipment. Each COM
object gets its own client and checks required interface capabilities before
equipment acquisition. Modern Connect/Disconnect return after local admission,
and Connecting reports both attachment progress and the host's completion/failure.
The legacy Connected setter waits for the bounded host operation. Completion
failures remain visible until another explicit connection operation. Getters
never launch/reconnect. Disposal closes only this client, leaving other leases
and the shared host running. Session and logical connection epochs fence responses
across reconnect/disconnect; an in-flight command is reported as possibly completed.

Saved selections bind instance, output and class, never an output list position.
CLSID is UUIDv5 in the URL namespace with name
`https://pulsarfab.com/regain/ascom-hub/output/{instance}/{output}/{class}` (lowercase
canonical UUIDs/class). The ProgID is `Rgn.HS.`, `Rgn.HM.` or `Rgn.HW.` plus the
32-digit CLSID, meeting COM's 39-character bound. Renames, paths and reordering do
not change identity. The server publishes one factory per saved selection. The
shared LocalComServer prepares default interface metadata on its STA before
publishing any factory and tracks weak COM object lifetimes.

Native ASCOM returns typed exceptions for unavailable weather, unsupported
properties/methods, invalid values and disconnects; it does not substitute NINA's
NaN convention. Other structured source errors retain their full upstream HRESULT
when present, using sanitized diagnostics. DeviceState retains scalar types and
does not invent a shared measurement timestamp. Live catalogue simulation metadata
replaces stale saved display metadata on attachment. Switch async capability is
false for the current scalar mapping; unsupported initiators fail explicitly.

Private registered fixtures exercise actual COM dispatch from both client
bitnesses to both server architectures, four outputs including two Switch outputs,
independent source leases and DeviceState collections. These fixtures do not
prove production chooser registration, SCM launch, interactive setup, full ASCOM
conformance or installed vendor/hardware acceptance. Those remain required before
merge and every broader plan gate.

Canonical native ASCOM ProgIDs for this instance's configured outputs are rejected
during configuration validation. The source factory also sends all derived output
CLSIDs to each isolated COM worker. Before activation, the worker reads the actual
ProgID-to-CLSID registration in its process architecture and activation context
(machine-only when elevated, otherwise merged HKCR), rejects any own class, and
activates that checked CLSID. A managed Type.GUID is not evidence of the registry
binding. Registered aliases are covered without invoking their constructor. A
failed source remains visible through diagnostics; connecting a scalar output
does not make the rejected source available. Different hub instances are permitted;
indirect cycles involving other hosts still require explicit topology safeguards.

Unix endpoint startup prepares the socket in a private temporary directory on
the endpoint filesystem, sets mode 0600, then renames it to the public address.
The process-wide umask is unchanged. Clients still reject public sockets with
broad permissions or the wrong owner; no permission denial is treated as readiness.
The publication regression passes on Linux x64/ARM64 and macOS Intel/ARM64 CI.

Registered export commands can specify `--bindings` with an existing absolute
saved-selection file and `--host` with an existing absolute host executable.
Explicit bindings suppress inherited host overrides; the host comes from the
registered command or this install. Neither argument silently falls back when
invalid. Duplicate options, missing files and relative paths fail before factories
are published. Reading bindings/metadata still acquires no host/equipment lease.
The optional fixture readiness path also records sanitized startup phases, with
no driver arguments or exception text. Private SCM tests verify cold launch and
shared factories against machine registration on disposable runners; d51abac
passed both complete CI runs, including x86/x64 cold SCM launch. Explicit launch
arguments alone do not establish production chooser support.

Production registration uses the existing `Regain.ASCOM.Register.exe` with
`/hubregister selections revision instance output ownerSid`, `/hubunregister
clsid ownerSid`, and installer-only `/hubunregisterall`. The same-user elevated
helper serializes machine edits and holds the saved-selection writer lock through
revision validation and publication. Both registry views point to this install's
x64 helper, explicit bindings/host paths and original user SID; the helper rejects
a different Windows identity before publishing factories. Metadata and registry
operations do not launch the host or acquire equipment.

Each output has its own AppID, chooser entry and versioned ownership inventory.
All views are checked before mutation, including collisions, user overlays,
changed commands, different owners/installs and newer registrations. A pending
inventory marks interrupted edits for explicit repair/removal; there is no
automatic mutation replay. Removal reads inventory even if selections/executables
are gone, preserving other installs. Caught failures attempt every rollback;
partial restoration is reported explicitly. Snapshots preserve value kinds,
owner/group/DACL and DACL protection, bounded to 512 keys, depth 16 and 1 MiB
across the operation. They do not capture SACL/audit metadata or provide a crash
transaction. Private-tree tests prove these behaviors. Actual production helper
publication/removal on the disposable runner, interactive setup, installer
lifecycle and conformance remain separate acceptance gates.

The existing setup executable's `/hubsetup` and Start menu entry open the shared
themed ASCOM manager. Its initial load reads saved choices and both machine
inventories; only explicit selector/editor actions attach to the host. Registration
requests are immutable, bind the observed selection revision, and invoke the
installed helper through same-account Windows elevation. UI blocks overlapping
actions and requires reload after errors/unknown completion. A 60-second helper
observation timeout never terminates or retries the edit. Orphan registrations
remain removable; foreign owner/install/file/newer-version entries are disabled.
Saved-choice removal is disabled while registry inventory still owns that choice.
Installer in-use checks also include both private hub helpers and shared DLLs.
Inno's pre-uninstall event invokes `/hubunregisterall` after confirmation and
in-use checks, before deleting files. Nonzero/unknown helper failure is fatal and
leaves application files for explicit recovery. Complete-install removal validates
all candidate outputs before any deletion and shares one rollback batch; other
installs (including future schemas) are excluded by their installation marker.
Batch removal admits at most 256 outputs, 4096 snapshot keys and 8 MiB; individual
edits retain the 512-key/1-MiB bounds. Inventory enumeration admits 4096 entries.
Crash atomicity and audit metadata preservation remain unclaimed. The actual
installer fixture covers upgrade preservation, conflicting commands preventing
file deletion, cleanup after selection deletion and another install's preserved
entries. A private installed COM metadata object also holds the nested helper
while upgrade/uninstall must refuse, without starting a Rust host or acquiring
equipment. Only its own RCW is released and the helper retires naturally. Those
machine cases require new disposable-runner CI before acceptance.
