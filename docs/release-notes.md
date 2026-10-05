# PulsarFab regain 0.5.9.0

Improves continuous ZWO frame delivery and SDK long-exposure waits. Existing
NINA, ASCOM and Alpaca single-exposure behavior is unchanged.

- Raise the SDK video-read wait cap from one second to five seconds, retaining
  exposure-derived shorter waits, normal-timeout backoff and the independent
  exposure-aware no-progress watchdog. Terminal SDK errors still fault.
- Serve cached stream status and latest-frame downloads independently of the
  camera owner. Blocking SDK reads no longer block these IPC operations; blocked
  output consumers still cannot stop camera draining. All hardware calls remain
  serialized on one owner, with one bounded latest-frame slot.
- Add an atomic stream poll for preview consumers.
- Allow explicit opt-in delivery of transition frames, marked settingsSettled=false.
  Keep the conservative settings fence for scientific/settings-sensitive consumers;
  legacy continuous clients retain settled-only delivery.
- Report raw arrival timing separately from delivered/settled frames. No camera
  identity or image content is added to diagnostics.
- Preserve delivery FPS limits, boundary-safe control edits, terminal faults,
  and cleanup. No extra camera discovery or implicit USB resets.

Automated validation uses simulated cameras and inert SDK ABI fixtures, including
a blocked acquisition call, slow/blocked output, transitional settings, FIFO-free
latest-frame delivery and terminal failures. Physical checks are operator-run,
not part of automated CI. These changes do not establish a firmware, power or
interference root cause for the reported SDK stall. Long soak, optical settings
accuracy, physical-disconnect recovery and Linux/macOS hardware remain unverified.

For Windows ASCOM and Alpaca use Regain-ASCOM-0.5.9.0-win-x64-setup.exe.
NINA plugin 0.5.9.0 is available through the two public plugin registries or
Regain-0.5.9.0.zip. Minimum NINA version and plugin identity are unchanged.
Rust version is 0.5.9. Windows release programs are signed by StackFoundry LLC.
Previously published assets remain immutable.
