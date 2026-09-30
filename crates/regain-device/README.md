# regain-device

One command-line tool and worker for every regain device driver:

```text
regain-device zwo camera-direct | camera-sdk | caa | efw | eaf
regain-device pegasus fc3
regain-device deepskydad ofp2
regain-device wanderer eta
regain-device usb reset|cycle TARGET
```

Accessory commands are `list-details`, `status` and `serve`, with
`--serial ID` and `--simulate`. `regain-alpaca` and the Windows plugins run
it as a worker process, one per session. See the
[architecture guide](https://github.com/pulsarfab/regain/blob/main/docs/architecture.md).

Part of [PulsarFab regain](https://github.com/pulsarfab/regain). Apache-2.0.
This project is not affiliated with the hardware vendors it supports.
