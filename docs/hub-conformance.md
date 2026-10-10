# Hub conformance checks

## Cross-kernel virtual-network checks

On Windows with an existing WSL distro, identify the Windows WSL virtual
interface index and run:

```powershell
Get-NetIPAddress -AddressFamily IPv6 | Select-Object InterfaceAlias, InterfaceIndex, IPAddress
cargo build -p regain-alpaca --locked -j2
python scripts/test-hub-wsl-network.py --distro Debian --interface eth0 --windows-scope 63
```

Replace 63 with this machine's WSL interface index. The distro needs its existing
Perl `IO::Socket::IP`/`IO::Select` modules and `ip`; the script installs nothing.
It starts a private Linux listener and production Windows Hub/publisher, checks
actual IPv4 and scoped link-local IPv6 traffic, and records server-side peer and
interface identities under `artifacts/hub-wsl-network-<id>/`. Simulated camera
pixels must match across JSON/ImageBytes and two clients, with one upstream
capture/download. Stopping the owned endpoint must withdraw cached safety.
Cleanup stops only owned processes; it never terminates the distro, changes
firewalls/routes, registers drivers or uses NINA. Do not run Python with `-O`.

This is same-machine traffic across separate Windows/Linux kernels and virtual
interfaces. It advances actual socket/scope acceptance beyond loopback fixtures;
it does not prove a physical LAN, UDP discovery, TLS, real equipment, installed
clients or OS sleep/wake. The [acceptance matrix](hub-acceptance.md) retains those
gates and links to the recorded results.

## Independent external inputs

To exercise an already running ASCOM OmniSimulator as an upstream application:

```powershell
cargo build -p regain-alpaca --locked -j2
python scripts/test-hub-omnisimulator.py --simulator-port 32323
python -m unittest discover -s scripts -p test_hub_omnisimulator.py
```

The harness accepts only a loopback port, checks the simulator's identity and
requires all eight supported devices disconnected with no pending connection.
It owns a separate Hub and HTTP publisher with pinned inputs, empty ordinary
camera profiles, no discovery, and one explicitly simulated native FocusCube3
gauge. It checks scalar/typed readback and a small camera exposure, comparing
exact JSON/ImageBytes pixels and a surviving second client. It restores camera
settings, releases its leases, waits for both modern connection flags to become
quiet, and stops only its owned processes. Do not run Python with `-O`.

Evidence is retained under `artifacts/hub-omnisimulator-<id>/`. A known invalid
external wheel offset array is recorded as a rejection while other checks
continue; the command still fails overall. The current external record and its
limits are in [the acceptance matrix](hub-acceptance.md). This is distinct from
ConformU validating Regain's outputs and from physical/installed-client acceptance.

## Regain output validation

Run external protocol and interface checks against an isolated simulated hub:

```powershell
cargo build -p regain-alpaca --locked -j2
python scripts/test-hub-conformance.py --conformu C:/path/to/conformu.exe
```

Use `--mode protocol` or `--mode interface` to run one suite, and `--classes
camera focuser` to select classes. The script always creates its own loopback
server, empty ordinary camera profiles and eight explicitly simulated sources.
Each mode gets a fresh host and frontend: protocol tests can acquire images, while
interface first-use tests require a camera that has not acquired an image yet.
Ordinary client disconnect continues to retain completed images.
It accepts no existing device configuration, upstream URI or COM ProgID. Never
rebuild the running server executable until the script finishes.

For native Windows ASCOM publication, use an unelevated terminal and matching
native helper builds under `target/debug/hub-ascom/{x86,x64}`:

```powershell
python scripts/test-hub-conformance.py --conformu C:/path/to/conformu.exe --mode interface --native-ascom x64
python scripts/test-hub-conformance.py --conformu C:/path/to/conformu.exe --mode interface --native-ascom x86
```

`--native-ascom` selects the server bitness. The validator retains its own build
bitness; the recorded runs below use the same 64-bit ConformU client against both
servers. Existing separate COM fixtures cover both client bitnesses. The runner
still owns a private loopback HTTP frontend for simulation controls, but all
conformance device calls go through COM and private host IPC.

Temporary per-user CLSID/ProgID aliases point to the production export classes.
ConformU's CLI requires a device-class suffix to infer the interface, whereas
Regain's stable 39-character ProgIDs preserve the complete output UUID. The
aliases change only CLI selection, not behavior, classes, bindings or tests.
No ASCOM Chooser/profile or production inventory registration is created.
The fixture owns its manually started COM server; its registry launch target is
deliberately absent so a failed server cannot spawn an unowned SCM replacement.
Cleanup stops that process, removes and verifies all owned registry roots in both
views, and writes `com-<bitness>-cleanup.json`, including after validator timeout.
This does not test installed-driver activation, signing, UAC or Chooser upgrades.

For native camera transport acceptance, select `--classes camera --camera-backend
sdk-simulated` or `--camera-backend direct-simulated`. These use only fixed private
simulation identities and models with `--simulate` on the host/frontend; native
workers inherit that explicit flag and never load the vendor SDK or open USB.
The runner accepts no model, serial or SDK-path override and records the worker
hash. Other sources remain the dedicated simulations. This exercises Regain's
native transport paths, not attached camera hardware.

Evidence is retained under `artifacts/hub-conformance-<id>/`: private configuration,
ConformU settings, host/server logs, per-class logs, interface JSON reports and a
combined summary including executable hashes and tool version. ConformU 4.5's
protocol command does not write its `--resultsfile`; the script records its exit
code and parses its explicit error/issue summary or exact zero-alert success
message instead. A missing summary, nonzero exit, configuration alert, timing
issue or reported error/issue fails
the run. The 900-second per-command bound stops that owned validator on timeout;
the script then records failure and continues the remaining checks. Cleanup stops
the two private processes it owns: HTTP frontend and separately launched hub host.
Closing an ordinary frontend does not stop a shared host.

The script enables strict Alpaca protocol checks and ConformU's full interface
tests. Switch settle delays are 20 ms for reads and 50 ms for writes because this
source has in-memory state. Tests, ranges and offsets remain enabled.
The panel's ordinary revision-checked simulation control sets cover travel to two
seconds so the validator's 500 ms sampling can observe motion before testing Halt.
The applied request and response are retained for each mode. This checks
simulated hub behavior, not physical settling or installed driver acceptance.
UDP discovery is disabled here and needs its separate acceptance check.

## Validator provenance

The initial checks use unmodified [ConformU 4.5.0](https://github.com/ASCOMInitiative/ConformU/releases/tag/v4.5.0),
source commit `49ab847c24c3d1a5bc11fb159ad2dd6787659098`, built with .NET 10 for
Windows. Its actual reported version is `4.5.0 (Build 55822.49ab847)`.
To build a private copy without installing it system-wide:

```powershell
git clone --depth 1 --branch v4.5.0 https://github.com/ASCOMInitiative/ConformU.git artifacts/conformu-source
dotnet build artifacts/conformu-source/ConformU/ConformU.csproj -c Release -f net10.0-windows
```

Keep the default relative build output: an upstream package's build target does
not handle an absolute overridden output path correctly. Build from a tagged Git
checkout so the validator reports its own source hash. An archive extracted inside
another repository otherwise picks up that parent repository's hash.

## Initial acceptance findings

The baseline full interface run uses the default ConformU settle delays:

| Class | Errors | Issues | Next action |
| --- | ---: | ---: | --- |
| Switch | 0 | 0 | Extend acceptance to other supported inputs and native outputs. |
| SafetyMonitor | 0 | 0 | Extend acceptance to other supported inputs and native outputs. |
| ObservingConditions | 0 | 0 | Extend acceptance to other supported inputs and native outputs. |
| Rotator | 0 | 0 | Extend acceptance to other supported inputs and native outputs. |
| FilterWheel | 0 | 0 | Extend acceptance to other supported inputs and native outputs. |
| Focuser | 0 | 4 | Reconcile boundary moves and per-move limits with the pinned interface and validator. |
| CoverCalibrator | 0 | 1 | Exercise HaltCover with a simulated travel duration observable by the validator. |
| Camera | 0 | 34 | Correct monochrome Bayer behavior and simulated capture times; review geometry setter expectations. |

There are no baseline timing issues or configuration alerts. These findings keep
the interface gate open. No physical device or installed vendor driver was opened.

The first tolerant protocol pass reports 36 issues across the eight classes:
unknown methods return HTTP 200 and incorrectly cased PUT keys are accepted. The
shared HTTP admission correction distinguishes unknown URLs (404), malformed
requests (400), and recognised methods that return an ASCOM error (200 with a
nonzero ErrorNumber). GET keys remain case insensitive; required PUT keys follow
the [Alpaca API Reference, section 2.2](https://ascom-standards.org/AlpacaDeveloper/ASCOMAlpacaAPIReference.html).
Incorrectly cased optional PUT client IDs are ignored. Error responses carry
`Value: null`, without inventing a measurement or completed image. Strict protocol
results are recorded separately from the interface findings above.

After the shared HTTP correction, ConformU 4.5's strict protocol suite passes all
eight simulated classes: zero errors and zero issues, with successful command
exit codes. Evidence is `artifacts/hub-protocol-conformu-third.log` and
`artifacts/hub-conformance-d178f97fbd0642a1b0d6cab1ea2df19a/summary.json`.
This passes the simulated HTTP protocol slice; the interface findings, other
source/backend combinations, native ASCOM conformance and broader acceptance
remain open.

## Camera and panel corrections (2026-10-07)

The selected full protocol/interface rerun passes Camera and CoverCalibrator:
zero errors, issues, timing issues and configuration alerts, with all interface
tests enabled. Evidence is `artifacts/hub-interface-camera-panel-conformu-second.log`
and `artifacts/hub-conformance-3d15f909af3c4cb1a07bd22877c0f4e9/summary.json`.
The five other previously passing classes have not been rerun in this selection;
the four focuser findings still need their own standards review and acceptance.

Camera corrections follow the [ASCOM camera interface](https://ascom-standards.org/newdocs/camera.html):
monochrome Bayer offsets are unsupported, three-plane RGB reports Color rather
than a Bayer mosaic, and exposure start metadata records actual UTC in the FITS
format `CCYY-MM-DDThh:mm:ss[.sss…]`. Exposure/readout and control deadlines still
use monotonic time. Positive scalar ROI settings can represent intermediate
desired geometry; combined sensor bounds are checked at StartExposure before
dispatch or pixel allocation. Invalid starts preserve the previous completed
image and leave the camera idle. Native capture retains its core bounds,
alignment, binning and exposure validation at that admission point.

Preserve the intermediate rerun in
`artifacts/hub-conformance-400ecc6d3eb448a1973bbf2278971651/summary.json`:
camera reports 22 issues after the initial production corrections. Five first-use
findings and the idle StopExposure finding came from reusing a host whose protocol
tests had already acquired an image. Sixteen timestamp findings came from an
RFC3339 `Z` suffix: ConformU 4.5 parses it into local time, then subtracts that from
a naive UTC value. The final emitted string follows the interface's implicit-UTC
FITS format, and the runner uses fresh modes; it does not clear retained images
on disconnect or modify the validator. The intermediate panel interface already
passed. Its protocol command exited successfully but used the alternative
zero-alert success sentence, which the runner now recognises explicitly.

These passes cover the private simulated Alpaca slice. Native ASCOM conformance,
other source/backend combinations and the original acceptance gates remain open.

## Focuser standards decision

The default simulated absolute focuser starts at 50000, has MaxStep 100000 and
MaxIncrement 1000. ConformU 4.5 issues direct moves to both endpoints without
respecting MaxIncrement, then expects targets below zero and above MaxStep to
clamp without an error. These account for the four retained findings.

The [published focuser interface](https://ascom-standards.org/newdocs/focuser.html)
defines MaxIncrement as a per-move limit and Move's InvalidValue error for an
out-of-range target; its MaxStep note also describes stopping at a limit. The
validator's expectations and the method contract therefore need reconciliation.
Regain currently rejects invalid targets and excessive travel before dispatch.
Existing `absolute_target_and_per_move_travel_are_separate_limits` coverage
verifies zero upstream writes for both failures and a valid boundary move.
Review against ASCOM.DeviceInterfaces 7.1.2 and the published IFocuserV4 reference
retains the method's InvalidValue behavior and advertised per-move bound. The
MaxStep note's automatic-stop wording conflicts with Move's out-of-range error;
it does not justify silently changing a requested target in a proxy. Regain
rejects the request instead of clamping, splitting or replaying movement. A client
must issue valid bounded moves itself. The unmodified validator's endpoint tests
remain reported as four issues; they are not converted into passes. This is a
documented standards discrepancy, with original external acceptance still tracked.

The pinned validator's `FocuserTester.cs` lines 500–611 contain the endpoint and
clamping expectations. Its ordinary move test at lines 694–709 does respect
MaxIncrement. Future validator versions must be reviewed afresh; this decision
does not exempt arbitrary errors or other movement failures.

## Native camera acceptance in progress

The first native SDK simulation run passes strict protocol checks and reports
eleven interface issues: unsupported StopExposure succeeds while idle, and ten
checks assume bin 3 must work when MaxBin is 4. This SDK simulation advertises the
actual sparse set `[1, 2, 4]`; the controller preserves that set and refuses bin 3.
The first direct simulation also passes protocol checks and reports only the idle
StopExposure issue. Both runs accept the new FITS timestamp property and report
no timing issues or configuration alerts.

Evidence: `artifacts/hub-conformance-9ff4cf9c56154468b427151baca0684b/summary.json`
(SDK) and `artifacts/hub-conformance-56e5fa94ff324876a1fa4833f79e039d/summary.json`
(direct). The idle optional-command behavior is being corrected; sparse binning
needs its own standards reconciliation. Native source modes are not declared
conformant from their protocol passes, and no bin is invented or hidden to make
the external tool pass.

After the idle command correction, native direct simulation passes full protocol
and interface checks with zero errors/issues/alerts/timing issues in
`artifacts/hub-native-direct-conformu-second.log` and
`artifacts/hub-conformance-adac0f3488d143e8a6cf12c03d0ece63/summary.json`.
Native SDK simulation passes protocol and retains only the ten sparse-bin findings
in `artifacts/hub-native-sdk-conformu-second.log` and
`artifacts/hub-conformance-cd0c6cf385d14787bf54ad84550644fa/summary.json`.
The SDK interface gate remains open. Neither native simulation opens hardware.

### Sparse binning standards decision

The [camera interface](https://ascom-standards.org/newdocs/camera.html#Camera.BinX)
allows InvalidValue for an unsupported bin. MaxBinX/MaxBinY describe the largest
supported factor in the current mode; they do not guarantee every intervening
integer. Regain preserves the SDK's `[1, 2, 4]` set, reports maximum 4 and rejects
3 before changing geometry. It does not hide bin 4, fabricate bin 3, remap the
requested factor or emulate binning in the proxy. ConformU 4.5 iterates every
integer through the maximum, so its ten bin-3 findings remain visible and failing
in the raw report. This decision preserves upstream capabilities; it does not
declare the SDK interface gate passed.

## Native ASCOM external checks (2026-10-07)

The full unmodified interface suite now runs against actual private native COM
exports. Results are retained separately from the earlier Alpaca checks:

| Server | Source mode | Result | Evidence directory suffix |
| --- | --- | --- | --- |
| x86 | Eight dedicated simulated classes | Seven classes pass; Focuser retains four endpoint issues; no timing/configuration findings | `547a9ca24e13412a86d3ddf984357db8` |
| x64 | Eight dedicated simulated classes | Six classes pass; Focuser retains four endpoint issues; panel has one DeviceState timing finding | `acfaf77a260b4f809f38571fe0a9836a` |
| x86 | Native direct camera simulation | Full camera interface pass, including image arrays; zero findings | `51973b71ab924addbb24d5f46d01cc29` |
| x64 | Native SDK camera simulation | Only ten sparse-bin findings; no errors, timing issues or configuration alerts | `f739010ae5744d088bb77f4356b9de04` |

These runs use `artifacts/hub-conformance-<suffix>/summary.json` plus individual
reports and executable hashes. The original x64 panel DeviceState call took
0.139 seconds against a 0.1-second target; retain that timing finding rather than
discarding it or relaxing the target. The isolated panel rerun passes with zero
findings in `b9497daca46342d68aae337350c386ff`; it does not prove the first timing's
cause. Forced-timeout and final cleanup evidence are recorded in the review log.
No installed vendor driver or physical device was opened.

### Panel timing diagnostics

To investigate the retained panel timing finding without changing the validator:

```powershell
python scripts/test-hub-state-timing.py --conformu C:/path/to/conformu.exe
```

Requires Windows, .NET 10 SDK and the built, unmodified ConformU 4.5.0 assembly
beside that executable, plus existing Regain host/x64 COM builds. It creates one
explicit simulated panel, a private host and temporary COM aliases using the
conformance publication helper. It accepts no existing config or device identity.
After the initial source poll is ready, six fresh clients measure the original
facade first in three processes, then
reverse the order in three more so getter/dispatch, collection enumeration and
value cleaning are measured cold too. `--native-ascom x86` selects the other
server architecture; the default is x64. All five expected state values are
validated; output, hashes and cleanup remain under `artifacts/hub-state-timing-*`.
Readiness uses source telemetry and never calls DeviceState before measurement.
It is bounded to five seconds and fails on source errors, incomplete polls or
generation changes. Connection acknowledgement alone does not seed the cache.
No HTTP listener, installed driver or hardware is used. No default toolchain or
upstream assembly is changed.

The command succeeds when measurement and cleanup complete; its timing output
is diagnostic, not a conformance pass. A fresh original-facade read took
17.79–29.13 ms in the current record, and split getters stayed below 3.58 ms.
The validator times enumeration/cleaning as well as the actual getter. This
narrows the measured path but does not explain or waive the original 139 ms
finding. Keep that raw report and obtain a trace of an actual slow call before
changing production behavior or declaring its cause resolved.

## Validator corrections and fresh timing, 2026-10-09

The [review patch](../scripts/conformu-review/README.md) corrects the two test
assumptions without changing Regain's motion or binning behavior. Focuser
endpoint tests now traverse using MaxIncrement and verify that invalid targets
raise InvalidValue without starting motion. Camera exposure tests recognize an
unsupported intermediate bin only when it was not previously accepted by the
property tests and the exception is specifically InvalidValue. Errors on bin 1,
the advertised maximum, previously accepted factors or any other exception
continue to fail. All supported bins undergo exposure and geometry checks.

The preparer pins the upstream source ZIP and source hashes, applies the patch
in a fresh directory, builds an explicitly labelled `4.5.0-regain-review`, and
records hashes. The runner requires `--validator-kind review`, verifies the
manifest against the binary/assembly/patch, and records review provenance.
Stock remains the default. This GPL-3.0 development tool is separate from the
Regain runtime and is not shipped in product packages.

Both corrected full interface suites pass with zero errors, issues, configuration
alerts and timing findings:

| Native COM server | Tested source modes | Evidence suffix |
| --- | --- | --- |
| x64 | Dedicated simulated focuser and native SDK camera simulation | `dcd5d1fa47e848d68e2bbcb2aea97eac` |
| x86 | Dedicated simulated focuser and native SDK camera simulation | `84797d8a186847bcaa8d2b3d3caff0c4` |

These are review-tool passes, not unmodified ConformU passes. The Move/MaxStep
wording discrepancy and upstream acceptance of the corrections remain separate.
The stock full eight-class x64 run in `a5d97828b50048da8240c1f49d43e90a` retains
exactly the four focuser findings; the other seven classes pass. Its panel
DeviceState takes 14 ms against the unchanged 100 ms target.

Cold-first timing diagnostics also pass their value/cleanup assertions in
`hub-state-timing-2399154ce4744658b4fa4e531a021db6` (x64) and
`hub-state-timing-64a83499149245c5bdc6321775fb84ba` (x86). Original facade first
reads take 19.23–28.35 ms and 19.77–34.18 ms respectively. Cold split driver/IPC
reads take 5.63–7.04 ms; enumeration takes 7.99–8.96 ms and cleaning 4.59–5.08 ms.
The original 139 ms sample has not recurred and still lacks a contemporaneous
trace. No causal production fix is claimed, no target is relaxed, and that
historical investigation remains open.

### Cached-state race and longer sweep

A 40-process diagnostic in `hub-state-timing-f1d3927979974838a3f9bab3470daf84`
failed its state-value assertion on client 11. The original error omitted the
returned values, so it cannot establish which field was missing. The harness
now records names, values and types on failure. Subsequent 40- and 100-process
sweeps (`edd77ed1cf024d47957f75d03f10be5a` and
`af7ae6c15cd440a887ee0e49e5e8263e`) passed all value and cleanup checks. These
later passes do not erase the failed run or prove its cause.

Inspection found a separate, reproducible ordering defect: typed DeviceState
and virtual-input reads obtained the query clock before cloning the cache. A
concurrent poll could then publish a newer sample, which the age validator
correctly rejected as future-dated. Commit `262d1bb` reads the snapshot first
and its source clock afterward across Camera, Focuser, Rotator, FilterWheel and
CoverCalibrator. The deterministic publication regression fails under the old
ordering and passes under the new ordering; genuine future timestamps still
fail. No I/O, motion replay, age clamping or timing waiver is introduced.
This fixes the demonstrated race; neither the earlier incomplete snapshot
report nor the original 139 ms timing sample establishes that race as its cause.


After the production ordering fix, unmodified ConformU passes the full panel
interface in both architectures: x64 `cf611be4dad84cddbc90f5621e019186`
(DeviceState 29 ms), x86 `da7dc5d6b5dc4a0780ba4b917a0e88b3` (23 ms).
Both have zero errors, issues, configuration alerts and timing findings.

The longer diagnostic still caught an empty **first** DeviceState collection
in `5be88958e56a4e42824c070c2d053f1c` (client 28) and
`c9a96e1d41784be0abddee908de2d2f3` (client 58). Failure-time telemetry in the
latter shows a healthy first poll completed immediately afterward, with all six
source properties and no errors. This exposed a separate harness assumption:
connection acknowledgement is not proof that the initial cache poll completed.
The harness now explicitly waits for that first poll via source telemetry,
without warming DeviceState, then requires all five correct operational values.
The failed runs remain retained; neither a startup empty cache nor a corrected
readiness precondition explains the historical 139 ms observation.


With that explicit readiness precondition, `2da53fac21404afe80fda7a61afdca57`
passes 100 fresh x64 clients, with facade-first reads of 17.75�30.79 ms.
Cold split getter, enumeration and cleaning maxima are 9.14, 8.83 and 5.90 ms.
The final x86 fixture, including the generation guard and client binary hashes,
passes six fresh clients in `f37e66a551b944d2b34bd918cdc267e4`; its facade-first
reads take 18.64�30.41 ms. All strict value assertions and cleanup pass. These
are diagnostic results; stock interface conformance is reported separately.
The required operational property list and its known-value qualification are
specified by [ASCOM DeviceState](https://ascom-standards.org/newdocs/covercalibrator.html#CoverCalibrator.DeviceState).
