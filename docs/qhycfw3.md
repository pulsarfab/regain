# QHY CFW3 filter wheels

Regain supports the **QHYCFW3 XL, nine positions, connected to the QHY600M
through its 4-pin cable**. The wheel uses the camera's existing connection in
both Direct USB and SDK mode. Direct mode uses the Rust QHY driver without
loading `qhyccd.dll`; SDK mode uses the installed QHY SDK.

A separate **QHY Filter Wheel (USB)** entry controls the wheel over its own
serial port, without a camera or QHY SDK. Both connections were tested with the nine-slot XL and firmware `20181114`.
See the [protocol and hardware evidence](qhycfw3-development.md).

## Standalone USB

1. Remove the camera control cable and select USB control with the recessed
   mode switch. A red startup flash indicates USB mode; green indicates 4-pin
   mode. Let startup rotation finish before connecting. See the
   [QHY setup guide](https://www.qhyccd.com/qhycfw3-filter-wheel/).
2. Select **PulsarFab regain QHY Filter Wheel (USB)** in NINA or ASCOM, open
   setup, and select its COM port. The generic CP210x USB serial (`0001` on the
   attached wheel) is not a unique device identity. Discovery lists candidate
   ports; connection verifies the wheel firmware and slot count.
3. In Alpaca, choose **QHY USB filter wheel setup**, select the port, and save.
   This exposes filter-wheel device 1. Disable the camera slot's 4-pin wheel
   option when using USB control.

USB mode reuses Regain's filter labels, focus offsets, styled accessory setup,
logging, and fresh optical-state checks when used with the Regain camera in
NINA. A move is submitted once;
Regain consumes its completion reply before querying position, then verifies
stable readback. Timeout or ambiguous completion stops further moves until
explicit reconnection. Calibration, direction control, and halt are not exposed.

ASCOM objects in the same application share a serial connection with independent
client leases. For sharing across applications, use one Alpaca server. Check
the configured port after changing USB wiring; identical bridges cannot be
distinguished by their USB serial numbers.

The sections below describe the separate **camera 4-pin** entries.

## NINA

1. In **PulsarFab regain Retryable Camera** setup, select the parent QHY camera,
   its serial number, and Direct USB or Vendor SDK.
2. Select **PulsarFab regain QHY Filter Wheel (camera 4-pin)** in NINA's filter
   wheel chooser. Either device may connect first.
3. Edit filter names, focus offsets, and exposure/autofocus settings in NINA's
   filter configuration. Existing slot settings are preserved on connection.

Both devices share one camera supervisor and USB handle. Disconnecting the
wheel leaves the camera usable. Disconnecting the camera disables its cooler
and any supported dew heater; the wheel can remain connected. Disconnect both
before changing the parent camera or backend.

## Native ASCOM

Pair **PulsarFab regain Retryable Camera N** with **PulsarFab regain QHY Filter
Wheel (Camera N)**, for slots 1–4. The wheel's setup opens the matching camera
profile, including backend selection, filter names, and focus offsets.

The paired COM objects share one local Rust process within the client
application. This is a native local pipe, with no Alpaca server required.
Separate applications do not share this in-process owner; use the same Alpaca
server when several applications need the camera and wheel.

## Alpaca

In the camera slot's setup, enable **Expose the QHY camera's 4-pin filter
wheel**, then save. The wheel appears in Alpaca discovery with device number
`camera number + 2`; EFW retains filter-wheel device 0 and standalone QHY USB
uses device 1. Its stable identity
follows the camera slot. Wheel setup links back to the parent camera setup.

Camera and wheel clients have independent connection leases. Both use the
camera slot's serial, backend, fallback policy, and recovery settings. Filter
names and focus offsets are saved in `filterWheel.names` and
`filterWheel.focusOffsets`; omitted slots default to `Filter N` and zero.

## Position and recovery

API positions are zero-based. Position is `-1` during motion. Regain requires
two matching readbacks separated by at least 250 ms before declaring a move
complete, with a 45-second movement deadline. Selecting the current position
does not send another motor command.

Moves are rejected during acquisition. Once the wheel is connected, the
camera verifies the expected filter before exposing and after downloading.
An unknown or different position prevents frame publication. Reconnecting a
camera worker preserves the expected filter; it reads the actual state rather
than replaying a move. A failed move is reported without an automatic retry.

Command timing and wheel responses use the camera's configurable transfer log
and circular diagnostic recorder.
