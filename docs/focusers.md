# Multiple Alpaca focusers

In current source/CI builds, open **Focusers and tilt** at `/setup/focusers`.
Add one slot per physical focuser, choose its model, then find and select its
serial number or port. Give each slot a useful name such as “Main focuser” or
“Guide focuser”. Two FocusCube3s or two EAFs use separate slots, just like mixed
models. ETA uses a focuser slot for common back focus and exposes individual
tilt points through setup and actions.

The server allocates an Alpaca device number when you add a slot. It identifies
the instance, not its model: ETA can be focuser 0 and an EAF can be focuser 3.
Numbers and UUIDs persist across restarts and unplugging devices. USB discovery
order never renumbers them. The setup page is `/setup/v1/focuser/N/setup` and
the client endpoint is `/api/v1/focuser/N/`, where N is the saved number.

Each slot owns its own worker and client connections. Selecting the same
physical device in two slots is rejected. Disconnect focusers of the model
being scanned before finding devices. Clearing a slot's device selection removes
it from management discovery while retaining its number and UUID for reuse.

## Existing configurations

When upgrading, existing EAF, FocusCube3, and ETA profiles keep their old numbers
(0, 1, and 2 respectively), UUIDs, selections, and settings. Only existing profile
files are imported. A fresh setup starts with no focuser slots. Later slots are
allocated after the highest saved number, including when migration leaves gaps.

With `--profiles cameras.json`, the slot list is `cameras.focusers.json` and new
profiles are `cameras.focuser-N.json`. Imported profiles continue using their
original `cameras.eaf.json`, `cameras.fc3.json`, or `cameras.eta.json` paths.
Back up these files together. Do not edit them while the server is running.

## Setup API

- `GET /setup/api/focusers` lists slots, models, profiles and connection state.
- `POST /setup/api/focusers` with `{"kind":"eaf"}`, `{"kind":"fc3"}`, or
  `{"kind":"eta"}` adds a slot and returns its number as `slot`.
- `GET` / `POST /setup/api/focusers/N` reads or updates the slot profile.
- `POST /setup/api/focusers/N/discover` finds hardware for that slot's model.
- `POST /setup/api/focusers/N/settings` updates device settings where supported.

Writes use JSON and the same-origin setup restrictions. The old model-specific
focuser setup API is replaced by these instance-specific routes. Native NINA and
ASCOM device selection is unchanged.
