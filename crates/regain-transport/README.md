# regain-transport

Serial I/O with bounded reads and writes, shared by the regain vendor crates.
Protocols keep their own identity checks, framing, pacing and retries. The
`usb` module resets one camera's USB device, or cycles its hub port, on Linux
and Windows. See [USB recovery](https://github.com/pulsarfab/regain/blob/main/docs/usb-recovery.md).

Part of [PulsarFab regain](https://github.com/pulsarfab/regain). Apache-2.0.
This project is not affiliated with the hardware vendors it supports.
