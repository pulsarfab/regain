# regain-alpaca

Alpaca server for ZWO cameras, CAA and Falcon V2 rotators, EFW filter wheels, EAF,
FocusCube3 and ETA focusers, and the OFP2 flat panel. Cameras can reread or
retake an image after a failed USB download. Add camera, focuser, and rotator
slots for multiple devices; each slot keeps a stable device number.

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

## Hub development preview

On the hub development branch, add `--hub-config ABSOLUTE_PATH` to publish the
configured Switch, SafetyMonitor, and ObservingConditions outputs through the
same HTTP server. This starts or attaches to the shared local hub host. Output
numbers and IDs come from the configuration; disconnecting an HTTP client does
not disconnect other frontends. Stopping the HTTP server leaves the host running.

For a hardware-free trial, copy the hub crate's `examples/simulated-observatory.json`
and pass its absolute path. Discovery labels these devices as simulation and
safety starts unsafe. Open `/setup/hub` to edit sources and outputs, inspect source
capabilities, review changes, and apply a new revision after disconnecting output
clients. Field definitions and validation come from the shared host.

The preview offers Switch v3, SafetyMonitor v3 and ObservingConditions v2, including
nonblocking Connect/Disconnect, Connecting, and cached DeviceState. Legacy
Connected remains available. Unavailable Switch/Weather readings are omitted from
DeviceState, and Switch channels report CanAsync=false. Asynchronous connection
failures remain visible until an explicit connect or disconnect. Credential and
recovery UI and conformance checks are still being implemented. After host loss,
explicitly restart the HTTP frontend; device
commands are never replayed. See [the hub plan](../../docs/hub-plan.md).

Part of [PulsarFab regain](https://github.com/pulsarfab/regain). Apache-2.0.
This project is not affiliated with the hardware vendors it supports.
