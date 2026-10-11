# PulsarFab regain 0.6.0.4

This patch improves direct camera cancellation and preserves cooling settings
through sensor initialization. The fixes apply to the shared camera backend
used by NINA, ASCOM and Alpaca.

## Camera cancellation and cooling

- Complete the ASI2600/6200 synchronization pulse before observing cancellation,
  then restore and verify long-exposure clock gates on every exit.
- Share clock restoration between ASI585/662/676 still and video stop paths,
  and verify acquisition is idle before acknowledging the stop.
- Attempt ordinary stop even if clock restoration fails. An uncertain cleanup
  retires the worker instead of reporting a clean cancellation and reusing it.
- Reapply acknowledged cooler and dew-heater settings after sensor setup,
  including setup failures. Preserve cooler demand, target, regulator history
  and the feedback deadline so repeated short captures do not delay regulation.
- Check pending cancellation before arming across all supported direct paths.
- Document register masks, write order, timing, thermal calibration and SDK
  evidence, with unknown preset meanings explicitly identified.

Original ASI6200MM Pro hardware passed twelve active cancellation stages,
each followed by a fresh full frame in the same worker. Real NINA camera API
checks passed caller cancellation and explicit abort with subsequent images,
unchanged workers and zero retries. ASI662MC passed early/gated still aborts
and gated video stop followed by fresh still captures. USB readbacks checked
clock gates, acquisition state and applicable thermal settings.

The remote ASI6200 lockup was not reproduced. These fixes close confirmed
software gaps; other camera revisions retain SDK-sequence and register-test
coverage and still need physical retesting. See
[cancellation acceptance](https://github.com/pulsarfab/regain/blob/v0.6.0.4/docs/asi6200-abort-cleanup.md)
and the [register reference](https://github.com/pulsarfab/regain/blob/v0.6.0.4/docs/camera-usb-registers.md).

## Installation and compatibility

Use `Regain-0.6.0.4.zip` for NINA or
`Regain-ASCOM-0.6.0.4-win-x64-setup.exe` for Windows ASCOM and Alpaca.
Windows programs and the installer are signed by StackFoundry LLC. This is a
package revision with rebuilt camera workers; it does not publish new Rust
crates. Minimum NINA version, plugin identity and settings are unchanged.

Regain Hub remains a preview. Its features and acceptance limits are unchanged;
see [Hub setup](https://github.com/pulsarfab/regain/blob/v0.6.0.4/docs/hub-setup.md)
and [Hub acceptance](https://github.com/pulsarfab/regain/blob/v0.6.0.4/docs/hub-acceptance.md).
Direct USB support remains experimental and model-specific; see
[supported hardware](https://github.com/pulsarfab/regain/blob/v0.6.0.4/README.md#supported-hardware).
