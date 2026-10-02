# regain-core

Camera session supervision for regain. It runs each camera in a separate
`regain-device` worker, bounds every request, restarts a stuck worker, and
recovers failed captures by rereading, reconnecting or retaking the exposure
within set limits. `regain-alpaca` and the Windows plugins build on it. See the
[recovery guide](https://github.com/pulsarfab/regain/blob/main/docs/sdk-lifecycle.md).

Current source also provides opt-in [shared white balance and AWB](https://github.com/pulsarfab/regain/blob/main/docs/white-balance.md)
for color Bayer cameras, with raw output preserved by default and explicit
corrected output. SDK and Direct USB pipe workers use the same engine.

Part of [PulsarFab regain](https://github.com/pulsarfab/regain). Apache-2.0.
This project is not affiliated with the hardware vendors it supports.
