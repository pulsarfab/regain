# PulsarFab regain 0.6.2.0

QHY600M cameras and QHYCFW3 XL filter wheels now work through Regain in NINA,
native Windows ASCOM and Alpaca.

- **QHY600M still capture:** choose Direct USB without the vendor SDK, or use
  an installed QHY SDK in an isolated worker. Both support photographic mode 0,
  full-resolution RAW16, bin 1, software ROI, gain, offset, USB traffic and
  cooling. Direct USB is validated for the U3G20 revision with firmware dated
  2024-10-18 on Windows. QHY images are consumed on download, so failed reads
  require an explicitly enabled replacement exposure rather than a reread.
- **QHYCFW3 XL wheels:** use the camera's 4-pin connection in either camera
  backend, or standalone USB serial without a camera or SDK. The nine-position
  wheel supports filter names, offsets and verified position readback. Shared
  camera ownership preserves independent camera/wheel connections; uncertain
  moves are not replayed, and changed optical state blocks exposure.
- **Direct USB video:** worker/pier clients can capture native RAW8 or RAW16 in
  photographic, high-gain, extended-full-well and 2CMS modes (0/1/2/3/8/9).
  Continuous draining and bounded latest-frame buffering tolerate slow consumers.
  Settings changes restart and settle the stream. Video supports exposures up to
  30 seconds and requires explicit stop/restart after a fault. NINA, ASCOM and
  Alpaca continue to expose still capture.
- **Cleanup and discovery:** failed cancellation retains acquisition state until
  backend cleanup succeeds. Busy or unsupported QHY devices no longer prevent
  discovery of other supported cameras; connection rechecks the selected identity.

Hardware acceptance covered NINA full-resolution FITS, native ASCOM and Alpaca,
both camera backends, and both wheel connections. The final direct video matrix
captured 171 distinct full-resolution images across all supported modes and bit
depths, blocked consumers, cancellation and return to still capture. All 1,206
integer gain/mode combinations match recorded SDK transfers. Other QHY camera
revisions, hardware binning, fiber-only modes and Linux direct camera USB remain
outside the validated scope. No QHY vendor SDK is bundled.

See [QHY600 setup](https://github.com/pulsarfab/regain/blob/v0.6.2.0/docs/qhy600.md),
[video modes](https://github.com/pulsarfab/regain/blob/v0.6.2.0/docs/qhy600-video.md),
and [filter-wheel setup](https://github.com/pulsarfab/regain/blob/v0.6.2.0/docs/qhycfw3.md).
Use `Regain-0.6.2.0.zip` for NINA or
`Regain-ASCOM-0.6.2.0-win-x64-setup.exe` for Windows ASCOM and Alpaca.
Rust workspace crates advance to **0.6.2**, including the new `regain-qhy` crate.
Existing ZWO recovery, USB reset, mount recovery, accessories and Hub features
remain included.
