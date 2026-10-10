# PulsarFab regain 0.5.14.0

Fixes cooling recovery and NINA cooling charts after retryable camera faults.

- Direct camera recovery immediately restores the last cooler power level and
  adds 8 percentage points per degree of measured warming, capped at 100%.
  The PI regulator responds more strongly and ramps output up four times faster.
  Output limits, bounded elapsed time, saturation protection and shutdown on
  missing temperature feedback remain enforced. Disabled cooling and a warmer
  requested target take precedence. SDK cooling remains controlled by the SDK.
- NINA retains the last temperature and cooler-power readings during reconnect
  and labels them **telemetry held**, avoiding invalid chart axes caused by NaN.
- NINA Driver Info uses a compact state, retry count and failure summary.
  Full errors and retry breakdowns remain in diagnostics and NINA logs at
  `%LOCALAPPDATA%\NINA\Logs`; search for `PulsarFab regain`.

On the attached ASI585MM Pro at a 15 C target, the baseline worker restart
reduced output from 15% to zero, warmed the sensor from 14.0 to 17.6 C and took
117 seconds to return a replacement image. The tuned run restored at least its
13% prior output, held 14.9–15.1 C and returned the replacement in 7 seconds.
These are two local runs with different starting temperatures and outputs,
not an overnight stability test or validation of every supported cooled model.

Regression checks cover chart bounds, compact status, restored and boosted
output, warmer targets, disabled cooling, malformed recovery requests,
controller limits, thermal models and shared-session worker replacement.

Use `Regain-0.5.14.0.zip` for NINA. The signed Windows ASCOM installer is
`Regain-ASCOM-0.5.14.0-win-x64-setup.exe`. Rust crates use version 0.5.14.
Hardware support and retry budgets are unchanged.
