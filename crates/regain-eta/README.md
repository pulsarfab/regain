# regain-eta

Independent Rust driver for Wanderer Astro ETA M54 serial tilt/back-focus control.
No vendor SDK or ASCOM dependency. See [protocol and setup](../../docs/eta.md).

```no_run
use regain_eta::{Tilter, serial::Serial};
let mut eta = Tilter::new(Serial::open("COM3")?)?;
println!("{:?}", eta.status()?.points_um);
// Only after checking mechanical clearance:
eta.move_to(500)?; // common back focus, µm; retains tilt
while eta.status()?.moving { std::thread::sleep(std::time::Duration::from_millis(250)); }
# Ok::<(), anyhow::Error>(())
```

Poll `status` to advance a sequential back-focus move and inspect `fault` before
accepting completion. The worker polls automatically. No physical halt command
is available; `cancel_queued` only cancels points that have not started.
