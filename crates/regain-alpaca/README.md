# regain-alpaca

Alpaca server for ZWO cameras, CAA rotators, EFW filter wheels, EAF,
FocusCube3 and ETA focusers, and the OFP2 flat panel. Cameras can reread or
retake an image after a failed USB download.

```sh
cargo install regain-alpaca regain-device
regain-alpaca --port 11111
```

Open `http://127.0.0.1:11111/setup`, select devices, and save. The server
starts `regain-camera` and `regain-device` from its own directory, so install
both crates to the same place. ZWO SDK camera mode also needs the SDK library
in that directory; Direct USB mode and accessories do not.
For LAN access add `--listen <host-LAN-IPv4>` and allow the HTTP port plus
UDP 32227 for discovery. See the
[Alpaca guide](https://pulsarfab.com/docs/regain/alpaca.html).

Part of [PulsarFab regain](https://github.com/pulsarfab/regain). Apache-2.0.
This project is not affiliated with the hardware vendors it supports.
