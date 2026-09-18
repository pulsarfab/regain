# README screenshots

The `alpaca-*.png` files are actual browser captures of the local Alpaca server built from
this repository. All devices are simulated; no physical camera or accessory is
used. The Simulation label remains visible in every image.

- `alpaca-camera.png`: camera slots, backend selection and discovery.
- `alpaca-recovery.png`: shared recovery defaults and timeout controls.
- `alpaca-rotator.png`: CAA selection, connection, position and motion controls.
- `alpaca-efw.png`: calibration, seven filter slots, names, focus offsets, and direction.
- `alpaca-eaf.png`: absolute focus, temperature, travel limit, and motor settings.

To regenerate, build the Rust workspace, then start a separate local instance
with a disposable settings file (the screenshot script changes its configuration):

```powershell
cargo build --release --locked
.\target\release\zwogain-alpaca.exe --simulate --no-discovery --port 11237 --profiles "$PWD\artifacts\readme-screenshots\cameras.json"
```

In another terminal with Node.js, Playwright and Google Chrome available:

```sh
node scripts/screenshot-alpaca.cjs http://127.0.0.1:11237 docs/images
```

The script requires a localhost server with simulation enabled. It captures
1280-pixel-wide pages, connects only simulated accessories, and disconnects them before
closing the browser. Stop the temporary server afterward.

The native EFW and EAF images are offscreen software renders of the production
WPF controls (`AccessorySetupWindow`), populated by the simulated native Rust
worker. They show the Filters and Settings tabs; `native-efw-calibration.png`
shows the Motion tab's calibration controls. The Simulation label is part
of the real dialog; no operating-system window frame is included. They are not
browser images or recreated mockups.

Regenerate on Windows after `cargo build`:

```powershell
dotnet run --project tools/ZwoGain.Screenshots
```

The helper detaches the actual dialog content and renders it with WPF
`RenderTargetBitmap`, preserving its resources and layout. Desktop capture was
unavailable in this session. Camera and CAA native captures remain outstanding.
Both screenshot helpers also exercise the simulated calibration button and
check that progress returns to idle before disconnecting.
