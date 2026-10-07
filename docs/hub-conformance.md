# Hub conformance checks

Run external protocol and interface checks against an isolated simulated hub:

```powershell
cargo build -p regain-alpaca --locked -j2
python scripts/test-hub-conformance.py --conformu C:/path/to/conformu.exe
```

Use `--mode protocol` or `--mode interface` to run one suite, and `--classes
camera focuser` to select classes. The script always creates its own loopback
server, empty ordinary camera profiles and eight explicitly simulated sources.
It accepts no existing device configuration, upstream URI or COM ProgID. Never
rebuild the running server executable until the script finishes.

Evidence is retained under `artifacts/hub-conformance-<id>/`: private configuration,
ConformU settings, host/server logs, per-class logs, interface JSON reports and a
combined summary including executable hashes and tool version. ConformU 4.5's
protocol command does not write its `--resultsfile`; the script records its exit
code and parses its explicit error/issue summary instead. A missing summary,
nonzero exit, configuration alert, timing issue or reported error/issue fails
the run. The 900-second per-command bound stops that owned validator on timeout;
the script then records failure and continues the remaining checks. Cleanup stops
the two private processes it owns: HTTP frontend and separately launched hub host.
Closing an ordinary frontend does not stop a shared host.

The script enables strict Alpaca protocol checks and ConformU's full interface
tests. Switch settle delays are 20 ms for reads and 50 ms for writes because this
source has in-memory state. Tests, ranges and offsets remain enabled. This checks
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
