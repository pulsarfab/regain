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
Full reconnect/re-exposure recovery is still managed by the .NET supervisor;
the Rust workers alone do not reproduce that complete plugin lifecycle.

## Build and test without a camera

Install stable Rust and the system C build tools (Xcode Command Line Tools on
macOS; a C compiler and linker on Linux). From the repository root:

```sh
cargo build --workspace --release --locked
cargo test --workspace --locked
python3 scripts/test-rust.py --bin-dir target/release
```

The Python test uses only simulated cameras. It checks both workers over pipes,
all supported direct camera/bin combinations, cooler controls, failed rereads,
and preservation of images after cleanup errors.

GitHub's **Build and test** workflow also builds and tests Linux and macOS on
x86-64 and ARM64. Its `zwogain-rust-*` artifacts contain both workers, without
the vendor SDK. These test builds are unsigned and are not macOS-notarized.
CI checks the SDK host against a small library built from the bundled C header,
including native `long` sizes and structure layouts.

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

Use the vendor SDK built for the same OS and CPU as the worker. The repository
currently bundles only the Windows DLL. Put the native library beside the host:

| System | Library name |
| --- | --- |
| Windows | `ASICamera2.dll` |
| Linux | `libASICamera2.so` |
| macOS | `libASICamera2.dylib` |

Or launch `zwogain-host --sdk /absolute/path/to/library`. SDK dependencies and
Linux device permissions must also be installed. Both workers use the same
version-1 JSON and binary protocol over stdin/stdout on every platform; see
[architecture](architecture.md).
