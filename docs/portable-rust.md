# Rust workers on Linux and macOS

The Rust workers build on Windows, Linux, and macOS. NINA and its plugin still
require Windows. Linux and macOS USB capture is experimental and needs camera
testing; a successful build or simulator test does not validate USB transfers.

| System | Direct USB transport |
| --- | --- |
| Windows | Installed ZWO/Cypress driver, overlapped I/O |
| Linux | Native usbfs through `nusb` |
| macOS | Native IOKit through `nusb` |

Linux and macOS need neither libusb nor the ZWO SDK for direct capture.
[`nusb`](https://github.com/kevinmehall/nusb) provides the OS USB bindings.
Windows keeps its existing driver; no WinUSB driver replacement is needed.

The transport claims the camera interface exclusively. It does not detach kernel
drivers or change the USB configuration. Camera setup, defect correction,
retained-frame rereads, and cooling code are shared across the three systems.
The `zwogain-core` Rust crate manages reconnects, cooler restoration, and
replacement exposures. Run `zwogain-alpaca` for this complete lifecycle and a
browser setup page; see [ASCOM instructions](ascom.md). The low-level worker
CLIs below remain useful for camera research and individual captures.

## Build and test without a camera

Install stable Rust and the system C build tools (Xcode Command Line Tools on
macOS; a C compiler and linker on Linux). From the repository root:

```sh
cargo build --workspace --release --locked
ZWOGAIN_TEST_WORKERS="$PWD/target/release" cargo test --workspace --locked
python3 scripts/test-rust.py --bin-dir target/release
```

The Python test uses only simulated cameras. It checks both workers over pipes,
all supported direct camera/bin combinations, cooler controls, failed rereads,
and preservation of images after cleanup errors.

GitHub's **Build and test** workflow also builds and tests Linux and macOS on
x86-64 and ARM64. Its `zwogain-rust-*` artifacts contain the Alpaca server, both workers, the matching
ASI SDK 1.41 library, licenses, build details, and SHA-256 checksums. Extract the
archive and use `./zwogain-rust/zwogain-direct` in place of
`./target/release/zwogain-direct` below. These test builds are unsigned and are
not macOS-notarized.
CI checks the SDK host against a small library built from the bundled C header,
including native `long` sizes and structure layouts. It also loads the real
vendor SDK and enumerates cameras, then tests capture and settings restoration
with a fake camera library. CI has no physical cameras.

The Linux packages are built and tested on Ubuntu 24.04. Older distributions
may need a local source build: for example, the prebuilt x86-64 worker cannot
run on Debian with glibc 2.28. macOS packages are built and tested on macOS 15.

## Linux permissions

The user running the direct worker needs read/write access to the camera's
`/dev/bus/usb` device. On a desktop with systemd/logind, install this rule as
`/etc/udev/rules.d/70-zwogain.rules`:

```udev
SUBSYSTEM=="usb", ENV{DEVTYPE}=="usb_device", ATTR{idVendor}=="03c3", TAG+="uaccess"
```

Run `sudo udevadm control --reload-rules`, then unplug and reconnect the camera.
For a headless machine, use a USB access group and a `MODE="0660"` rule instead.
Close other camera programs before opening the worker. An access or busy error
should be resolved before capture; the worker will not take ownership by force.

## First camera test

Cap the camera and start with a short, small frame. Connect just the camera being
tested. Save the probe output and stderr alongside the capture output:

```sh
./target/release/zwogain-direct --probe-all > probe.json
./target/release/zwogain-direct --capture-6200 \
  --width 256 --height 256 --microseconds 100000 --frames 3 \
  --stream > dark-frames.bin 2> capture.log
```

Choose the capture switch for the attached device:

| Camera | Switch |
| --- | --- |
| ASI6200MM Pro P25 | `--capture-6200` |
| ASI2600MM Pro P25 | `--capture-2600-p25` |
| ASI2600MM Pro, non-P25 | `--capture-duo` (legacy command name) |
| ASI676MC | `--capture` |
| ASI220MM Mini guide | `--capture-guide` |

Each streamed frame contains a four-byte little-endian JSON length, that JSON,
then RAW16 pixels. The capture metadata reports the image dimensions. Omitting
`--stream` prints metadata only. The CLI does not enable cooling; use the worker
protocol to test cooler controls.

Then test full frames, bins, selected image areas, and longer exposures. Pay
particular attention to the final USB chunk when its size is not a multiple of
the endpoint packet size. Native reads request a rounded-up buffer but accept
only the exact expected number of image bytes. Short, extra, failed, or timed-out
data is rejected; retained-frame recovery starts again from byte zero.

For main-camera reread testing, use `--read-retries 2` with
`--interrupt-read-after-bytes 1048576`. Test cooling and recovery separately after
ordinary captures work. On a USB timeout the worker cancels the transfer and
waits up to two seconds for terminal completion. If cancellation does not finish,
it exits without freeing buffers still owned by the OS.

Before calling a platform hardware-tested, compare SDK and direct dark frames,
exercise every bin and controls, check repeated captures and same-frame rereads,
then test cooler shutdown and reopening. Physical disconnect recovery remains a
separate test. Save the OS version, CPU architecture, camera serial, USB probe,
capture logs, and pixel samples with the results.

## SDK host

The native packages include the SDK. On Debian/Ubuntu, install its runtime
dependency with `sudo apt install libusb-1.0-0`. macOS packages include libusb;
Homebrew is only needed when building the package yourself. The direct driver
does not use libusb.

For large SDK frames on Linux, ZWO recommends a 200 MiB USB transfer-memory
limit. Check `cat /sys/module/usbcore/parameters/usbfs_memory_mb`; if needed,
`echo 200 | sudo tee /sys/module/usbcore/parameters/usbfs_memory_mb` changes it
until reboot. This is a system-wide setting; our scripts do not change it.

From an extracted package, these commands work without NINA or Python:

```sh
./zwogain-rust/zwogain-host --list
./zwogain-rust/zwogain-host --inspect --camera "ZWO ASI6200MM Pro"
./zwogain-rust/zwogain-host --capture --camera "ZWO ASI6200MM Pro" \
  --width 256 --height 256 --microseconds 100000 --frames 3 \
  --gain 100 --offset 50 --output sdk-darks
```

The output directory must be new. Each image has a `.raw` file containing
little-endian RAW16 pixels and a `.json` file with geometry and capture settings.
Dark capture is the default; use `--light` for light frames. Use `--serial` when
two cameras share a name. `--help` lists all options.

`--inspect` and `--capture` accept `--set CONTROL=VALUE`. For example, add
`--set 16=-10 --set 17=1 --hold-seconds 60` to set the cooler to -10 C and wait
60 seconds before inspecting or capturing. A fixed hold does not guarantee the
camera has reached its target. The worker restores settings changed with
`--set`, `--gain`, or `--offset`, including their prior automatic mode, before
closing. Restoration errors are reported. A hung SDK or forced process exit
can prevent restoration; the CLI watchdog reports this when it terminates.

Failed downloads get up to two same-frame retries while the SDK reports the
frame ready (`--read-retries` changes the count). This CLI does not automatically
reconnect or take replacement exposures. The NINA supervisor provides that
full recovery lifecycle.

For a source build, install `libusb-1.0-0` on Debian/Ubuntu or run
`brew install libusb` on macOS, then stage the SDK beside the release workers:

```sh
cargo build --workspace --release --locked
python3 scripts/stage-sdk.py target/release --check
./target/release/zwogain-host --list
```

The repository contains these platform libraries:

| System | Library name |
| --- | --- |
| Windows | `ASICamera2.dll` |
| Linux | `libASICamera2.so` |
| macOS | `libASICamera2.dylib` |

Use `--sdk /absolute/path/to/library` to override the bundled library. Linux
device permissions must also be installed. Both workers use the same
version-1 JSON and binary protocol over stdin/stdout on every platform; see
[architecture](architecture.md).
