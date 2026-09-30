# regain-worker

Bounded newline-delimited JSON IPC for regain accessory workers. Requests over
4 KiB, a partial final line, or end of input end the session. Camera workers use
a separate length-prefixed protocol.

Part of [PulsarFab regain](https://github.com/pulsarfab/regain). Apache-2.0.
This project is not affiliated with the hardware vendors it supports.
