# regain-hub

Shared device aggregation, safety policy, and configuration for
[PulsarFab regain](https://github.com/pulsarfab/regain).

Under development. The shared engine is independent of NINA, COM, and HTTP;
frontend support is tracked in the repository's
[hub plan](https://github.com/pulsarfab/regain/blob/main/docs/hub-plan.md).
Do not infer frontend or hardware support from these library contracts.

Safety behavior is adapted from the Apache-2.0 NINA Field Kit reference at
`8be3d38f0b04fa78d7ae36b460ed10656f259d0f`, with additional observation fencing
and per-consumer cadence. Relevant configuration changes clear safe permission.

Apache-2.0; see LICENSE. Copyright 2026 Yann Ramin.
