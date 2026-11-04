# regain-qhy

QHY600M support for PulsarFab regain: a pure Rust Windows USB backend and a
dynamically loaded SDK backend, integrated with NINA, native ASCOM and Alpaca.
Current scope is the traced QHY600 U3G20 revision, photographic mode 0, RAW16,
bin 1, gain/offset/traffic, software ROI and bounded cooling. Other revisions
must use the SDK until their direct protocol is validated.

Downloads are consumptive. Failed images are discarded; the common supervisor
can reconnect and take an explicitly reported replacement exposure. Same-frame
rereads, video, hardware binning and privileged USB reset are not advertised.

The vendor SDK is loaded dynamically in the isolated device worker. Install the
QHY system driver before discovery; the SDK DLL alone is insufficient on Windows.

The default SDK path is `C:\Program Files\QHYCCD\AllInOne\sdk\x64\qhyccd.dll`;
override it with `REGAIN_QHY_SDK`. No vendor SDK is bundled by this crate.
See [setup and limits](https://github.com/pulsarfab/regain/blob/main/docs/qhy600.md)
and [protocol evidence](https://github.com/pulsarfab/regain/blob/main/docs/qhy600-development.md).

CFW3 wheel support includes the shared camera 4-pin connection and a standalone
USB serial worker (`regain-device qhy cfw`). The latter uses an explicitly
selected port, no QHY SDK, and never replays a move after an uncertain reply.
See [wheel setup and validation](https://github.com/pulsarfab/regain/blob/main/docs/qhycfw3.md).
