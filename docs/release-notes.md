# PulsarFab regain 0.5.15.1

Adds optional host USB read sizing for SDK-less cameras.

- **Direct USB read size (KiB)** is available in NINA Advanced settings,
  native ASCOM Timeouts, and Alpaca camera recovery settings. The shared key is
  `recovery.directReadChunkKiB`; the CLI option is `--read-chunk-kib`.
- Choose powers of two from 1 to 1024 KiB. The default remains 1024 KiB
  (1 MiB), including for existing configurations. SDK mode ignores this option.
- The size stays consistent through retained-frame retries and worker replacement.
  Transfer diagnostics report request size, request count and elapsed read time.
- Smaller host reads add overhead. This control is separate from the SDK's
  camera-side USB Traffic percentage; it does not select USB 2 or set a fixed
  MB/s limit. See [USB read sizing](transfer-recovery.md#limit-the-size-of-direct-usb-reads).

Acceptance used the attached ASI585MM Pro over USB 3 on Windows: 21 full-resolution
captures across 1, 4, 16, 64, 256 and 1024 KiB and the omitted default. Each capture
recovered an injected host read interruption and verified a byte-identical retained
replay after another partial read. This is not physical USB 2 validation or a claim
that smaller reads improve fault recovery.

Includes the 0.5.14 cooler-recovery, retained NINA cooling telemetry and compact
retry-status fixes. Hardware support and retry budgets are unchanged.
The Hub remains on main for the 0.6 train and is not included in this 0.5 release.

Use `Regain-0.5.15.1.zip` for NINA, or the signed Windows ASCOM installer
`Regain-ASCOM-0.5.15.1-win-x64-setup.exe`. Rust crates use version 0.5.15.


Managed telemetry stays at its last measured temperature and cooler power while
opening a replacement worker. Open-response defaults no longer briefly replace
those readings before fresh telemetry arrives. The NINA recovery acceptance
fixture now holds the reconnect phase explicitly instead of racing fresh reads.
