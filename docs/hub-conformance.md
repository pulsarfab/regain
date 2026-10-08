# Hub conformance checks

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
