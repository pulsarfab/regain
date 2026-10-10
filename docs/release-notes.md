# PulsarFab regain 0.5.13.1

Fixes cooling-chart axes disappearing in NINA after a retryable camera fault.
During reconnection the driver retains the last measured temperature and cooler
power and marks them **telemetry held**. Fresh readings resume after recovery;
held values do not imply that camera temperature stayed constant.

NINA Driver Info now shows a compact recovery state, retry count and shortened
last failure. Full failure messages and retry breakdowns remain available in
diagnostics and NINA logs under `%LOCALAPPDATA%\NINA\Logs`. Search the logs for
`PulsarFab regain`; the README and camera guide explain how to inspect them later.

Regression checks reproduce the invalid chart bounds with the previous code
and cover both camera recovery paths, worker replacement through the Rust
supervisor, zero cooler power, updated readings and bounded status text.

Use `Regain-0.5.13.1.zip` for NINA, or update through either public plugin feed.
The signed Windows ASCOM installer is `Regain-ASCOM-0.5.13.1-win-x64-setup.exe`.
This is a .NET/plugin revision: Rust crates remain at 0.5.13 and need no new
crates.io publication. Hardware support and recovery limits are unchanged.
