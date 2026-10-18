# regain-core

Camera session supervision for regain. It runs each camera in a separate
`regain-device` worker, bounds every request, restarts a stuck worker, and
recovers failed captures by rereading, reconnecting or retaking the exposure
within set limits. `regain-alpaca` and the Windows plugins build on it. See the
[recovery guide](https://github.com/pulsarfab/regain/blob/main/docs/sdk-lifecycle.md).

Current source also provides opt-in [shared white balance and AWB](https://github.com/pulsarfab/regain/blob/main/docs/white-balance.md)
for color Bayer cameras, with raw output preserved by default and explicit
corrected output. SDK and Direct USB pipe workers use the same engine.

`accessory::AccessoryWorker` is the shared client for the accessory workers in
`regain-device`. It bounds newline-JSON messages, serializes requests through an
exclusive mutable handle, and retires the child after timeout, cancellation, or
invalid framing. A framed worker error is distinct from transport
failure, but either may follow a dispatched command. Neither is replayed. Windows child ownership uses the
same kill-on-close job support as camera workers.

Part of [PulsarFab regain](https://github.com/pulsarfab/regain). Apache-2.0.

`focuser::Controller` provides opt-in continuous temperature compensation,
absolute reference tracking, noise filtering and one-owner backlash plans for
EAF and FocusCube3 workers. Its generated configuration schema serves native
NINA/ASCOM, Alpaca and direct Hub sources. Explicit moves remain supported with
TempComp enabled; Halt disables tracking. See the
[focuser guide](https://github.com/pulsarfab/regain/blob/main/docs/focuser-temperature-compensation.md).
This project is not affiliated with the hardware vendors it supports.
