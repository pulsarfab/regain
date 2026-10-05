# regain-zwo

ZWO ASI camera backends, CAA rotator, EFW filter wheel and EAF focuser.

Features: `asi-direct`, `asi-sdk`, `caa`, `accessories` (all enabled by default).
The `hid` module is shared by ZWO accessories.

The [ASI585MM Pro backend](https://github.com/pulsarfab/regain/blob/main/docs/asi585mm-pro.md)
shares the same capture, calibration, replay, and video machinery, with a mono
sensor profile, software bins 1–4, and DAC-based cooling. No vendor SDK, additional
crate, or separate executable is required. The model exposes no heater control.

The direct ASI662MC and ASI676MC backends share Bayer capture, calibration, and
retained-frame recovery with separate traced sensor profiles. ASI662MC supports
RAW16 bin 1; short captures use a roughly 100 ms frame interval while preserving
integration time. See the [ASI662MC guide](https://github.com/pulsarfab/regain/blob/main/docs/asi662mc.md).

CAA example: No ZWO SDK or hidapi C library is
required. Uses Windows HID, Linux hidraw, or macOS IOKit. Windows hardware
validation covers CAA-M54 firmware 1.1.1; Linux/macOS hardware testing is pending.

```rust,no_run
use regain_zwo::caa::{Caa, transport::{enumerate, Device}};
let devices = enumerate()?;
let device = devices.first().ok_or_else(|| anyhow::anyhow!("no CAA"))?;
let mut caa = Caa::connect(Device::open(device)?)?;
println!("{:?}", caa.status()?);
# Ok::<(), anyhow::Error>(())
```

Also provides the `regain-device zwo caa` command-line tool: `list`, `list-details`, `status`,
`serve`, and `exercise`. Select a device with `--serial SERIAL`. The JSON worker
supports motion, sky-angle sync, mechanical origin reset, settings and explicit
segmented travel. Exercise moves within eight degrees of the initial position
and restores settings. See the [protocol and usage guide](https://github.com/pulsarfab/regain/blob/main/docs/caa.md).

Apache-2.0. The NTC table has the separate notice in `LICENSE-ZWO`.
This project is not affiliated with ZWO.
