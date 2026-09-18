# zwogain-caa

Rust USB HID driver for the ZWO CAA rotator. No ZWO SDK or hidapi C library is
required. Uses Windows HID, Linux hidraw, or macOS IOKit. Windows hardware
validation covers CAA-M54 firmware 1.1.1; Linux/macOS hardware testing is pending.

```rust,no_run
use zwogain_caa::{Caa, transport::{enumerate, Device}};
let devices = enumerate()?;
let device = devices.first().ok_or_else(|| anyhow::anyhow!("no CAA"))?;
let mut caa = Caa::connect(Device::open(device)?)?;
println!("{:?}", caa.status()?);
# Ok::<(), anyhow::Error>(())
```

Also provides the `zwogain-caa` command-line tool: `list`, `list-details`, `status`,
`serve`, and `exercise`. Select a device with `--serial SERIAL`. The JSON worker
supports motion, sky-angle sync, mechanical origin reset, settings and explicit
segmented travel. Exercise moves within eight degrees of the initial position
and restores settings. See the [protocol and usage guide](https://github.com/theatrus/zwogain/blob/main/docs/caa.md).

Apache-2.0. The NTC table has the separate notice in `LICENSE-ZWO`.
This project is not affiliated with ZWO.
