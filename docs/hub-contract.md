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
not connect sources until clients request outputs. Native NINA/ASCOM and ordinary
Alpaca attachment are still to be implemented against this same local host.
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
host and configuration replacement are integrated; frontend launch/attach and
resume handling remain open.

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
sourceStatus, hostStatus, connect, disconnect, and typed get/put for Switch, SafetyMonitor,
and Weather. `validateConfig` reports persisted configuration/relationship errors
with `scope: "configuration"`; it does not authorize durable apply or establish
live hardware capabilities. `getConfig` retains credential references for local
editing, but contains no credential values. The executable's persistent service
also advertises `applyConfig`. A read-only embedded runtime does not advertise it
and returns unsupported. Camera image transport remains a separate pending feature.

Requests start in arrival order but do not wait for earlier I/O to complete.
This lets Disconnect cancel a pending Connect while cached safety reads remain
responsive during another request. There are at most eight in-flight operations
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
lifetime. Frontend automatic launch/attach and reconnection remain pending.

Windows tests cover anonymous denial,
permissive-storage rejection, cross-process contention/crash recovery, and actual
IPC. Endpoint commit `0c8bfe7` also passed Linux x64/ARM64 and macOS Intel/ARM64 CI,
including Unix permission/link/socket-cleanup fixtures. The host integration's
portable checks are separate and must pass before this checkpoint is complete.

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
COM, virtual-output, simulated-interface, and native-camera source construction
remain explicit later implementation work, with no fallback to another backend.

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

Field Kit reference commit `8be3d38f0b04fa78d7ae36b460ed10656f259d0f` is
Apache-2.0. Its endpoint state, aggregate, service, and integration tests supply
behavioral cases; Regain adds cadence, generation, configuration, and multi-output
ownership tests. Record attribution in the shared crate and third-party notices.
