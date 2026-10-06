# PulsarFab regain 0.5.11.0

Maintenance release from the 0.5 branch. Continuous SDK and Direct frames now
carry explicitly estimated exposure timing for consumers such as AutoPierCam.

The acquisition owner records host receipt time before IPC delivery. A start
estimate subtracts exposure duration, with an additional full exposure or
observed raw-frame interval as a conservative freshness margin. Unsettled
transition frames carry no estimate. This is not a sensor timestamp or a
guaranteed bound on native buffering, readout or clock adjustments.

No capture command, exposure cancellation, stream restart, hardware discovery,
or scientific single-exposure behavior changes. Hardware-free tests cover
30/60-second calculations, missing/transition timing, and metadata delivery
through SDK and all Direct simulators. No new physical-camera validation is
claimed.

Use the signed Windows ASCOM/Alpaca installer
`Regain-ASCOM-0.5.11.0-win-x64-setup.exe`, or the NINA package
`Regain-0.5.11.0.zip` through either public plugin feed. Minimum NINA version
and plugin identity are unchanged. Rust workspace version is 0.5.11.
