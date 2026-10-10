# Physical camera cancellation and cooling acceptance

Tested on 2026-10-10 with the attached **ZWO ASI585MM Pro**, serial
`2805960a19020900`, using the real NINA `ResilientCamera` implementation and
supervised Rust workers. Both SDK 1.41 and SDK-less direct USB are exercised.
The test invokes the plugin API; it is not an installed NINA GUI test. Only the
image factory is substituted to inspect the downloaded pixels and metadata.

The opt-in test is `CameraHardwareTests.Asi585LiveCoolingAbortAndRestartThroughNina`.
It rejects another serial before changing controls, leaves cooling off, and
disconnects its own camera session in cleanup. Ordinary CI skips it.

`Asi585DisconnectDisablesThermalControls` also passes in both backends: enable
the cooler through NINA, disconnect while idle or exposing, then inspect its actual USB enable flag
using a fresh direct worker. The flag is off after SDK and direct disconnects.
The ASI585MM Pro advertises no controllable dew heater. Supported heater shutdown
and independent attempts after one actuator fails are covered by simulated and
capability/readback tests; they are not physical heater acceptance on this model.

```powershell
$env:REGAIN_TEST_ASI585_HARDWARE = '1'
dotnet test tests/Regain.NINA.Tests -c Release --filter CameraHardwareTests `
  --logger 'trx;LogFileName=asi585-hardware.trx'
Remove-Item Env:REGAIN_TEST_ASI585_HARDWARE
```

Build the debug Rust workers first and provide `vendor/zwo/ASICamera2.dll` for
the SDK case. The physical camera must be idle and available to Regain.

## What this checks

- Change the cooler target while a 600-second exposure is running; require an
  acknowledged readback and measured cooling. SDK open defaults are not a
  measured starting temperature, so allow its hardware poll to refresh first.
- Explicitly abort, wait for cleanup, preserve the worker PID and enabled
  cooler, then download a new 64 × 64 RAW16 image with the actual model name.
- During another long exposure, request a warmer target, turn cooling off and
  cancel the readiness caller. Direct mode must reduce unwanted output to zero.
  A subsequent capture must succeed without a replacement exposure or worker.

The long exposures are deliberately interrupted. These results do not claim
completed 600-second captures, physical USB-disconnect recovery, or convergence
on an ASI6200 or at a deep subzero target.

## Retained evidence

Both cases passed in `asi585-hardware-final.trx`:

| Measurement | SDK | Direct USB |
| --- | --- | --- |
| Colder target acknowledgement during exposure | 34 ms | 125 ms |
| Starting measured temperature | 18.0 °C | 16.5 °C |
| Last logged temperature / elapsed sampling | 16.8 °C / 86 s | 11.6 °C / 41 s |
| Requested colder target | 12 °C | 10 °C |
| Explicit abort, including cleanup | 243 ms | 111 ms |
| Cooler output before / after abort | 19% / 19% | 28% / 28% |
| Subsequent images after both cancellation paths | 64 × 64 RAW16, correct model | 64 × 64 RAW16, correct model |

These are sequential runs with different starting temperatures and vendor
regulators; they are not a controlled backend performance comparison. SDK
regulation ramps slowly and remains vendor-owned. The direct result demonstrates
the immediate demand change and continued convergence on this attached camera.

TRX output includes the selected identity, backend, target acknowledgement time,
temperature/power samples, abort latency and before/after output, plus image
dimensions, mean and model. Local artifacts are kept under `artifacts/`; they
are not distributed as part of the plugin.

For worker-loss recovery, `scripts/inspection/validate_cooler_recovery.py`
records JSONL samples, worker hashes and diagnostics. It terminates only its
owned worker during a short exposure, checks replacement capture recovery,
and disables cooling before exit. See [cooling recovery tuning](transfer-recovery.md#cooling-recovery-tuning).

The 2026-10-10 direct run at a 15 °C target had 15.3 °C / 11% output before
worker loss. The replacement worker resumed 11.6% output at 15.38 °C, rose to
13% as the temperature reached 15.5 °C, and delivered
the replacement image in 8.02 seconds. After another 30 seconds it read
15.1 °C / 11%. This compares with the earlier probe's 117-second recovery after
output fell to zero; ambient conditions and steady loads were not controlled
between runs. Evidence: `asi585-recovery-20261010-final/samples.jsonl` and
`worker.log`, including worker hashes and recovery events.
