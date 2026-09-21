# Third-party notices

PulsarFab regain is copyright 2026 Yann Ramin, Apache License 2.0 (see `LICENSE`).
The original camera/recovery logo in `assets/` and the embedded PNG are covered
by the same license.

The Rust C ABI declarations in `crates/regain-zwo/src/asi/sdk/raw.rs` were adapted
from [AutoPierCam](https://github.com/theatrus/autopiercam), copyright 2026 Yann
Ramin, Apache License 2.0. AutoPierCam also informed the package and process
architecture.

The ZWO ASI SDK DLL and header are copyright 2015 ZWO Company and distributed
under the MIT-style license in `vendor/zwo/LICENSE.txt`, included in packages
as `licenses/ZWO-ASI-SDK.txt`. ZWO's camera device driver is not included.

The native CAA driver's NTC resistance table comes from CAA SDK 1.5.9,
copyright 2015 ZWO Company, under the MIT-style license in
`crates/regain-zwo/LICENSE-ZWO`. Distributed packages include that license.
The CAA driver contains no SDK binary and calls the operating system's HID API.

Native worker packages include ASI SDK 1.41 for their OS and architecture;
source hashes and licenses are in `vendor/zwo/native/`. macOS packages also
include libusb under LGPL-2.1-or-later, with its license and the source URL for
the packaged version in `libusb-source.json`. It remains a separate, replaceable
dynamic library. Linux packages use the system libusb installation.

Rust dependencies retain their original licenses. The build script packages
their license/copyright texts and the Rust standard-library copyright bundle
under `licenses/`. Cargo.lock records exact versions.

The OFP2 and FocusCube3 drivers use the unmodified `serialport` Rust crate under MPL-2.0.
Its corresponding source is available from
[crates.io](https://crates.io/crates/serialport/4.10.1) and
[the upstream repository](https://github.com/serialport/serialport-rs).
The Deep Sky Dad ASCOM driver was inspected to determine serial commands;
no vendor driver code, binary, or firmware is included in either serial crate.
FocusCube3 was implemented from Pegasus's protocol reference, local driver
inspection, and serial traces; Pegasus binaries are not redistributed.

The Windows COM package includes ASCOM DeviceInterfaces, Alpaca Components,
Common Components, and Exception Library (ASCOM Initiative), plus Microsoft
.NET support libraries. These packages declare the MIT license. Their copyright
notices, license texts, package metadata, and supplied third-party notices are
included under `licenses/dotnet/`. The .NET Framework itself is not bundled.
The Rust Alpaca server does not use these .NET libraries.

NINA's assemblies and .NET runtime are supplied by the existing NINA
installation, not bundled in this plugin. NINA is licensed under MPL-2.0;
its public interfaces and native driver behavior were used as implementation
references. NINA SDK packages are development-only references.

The separate camera exercise kit bundles Python, Frida and a PyInstaller
bootloader, plus their runtime dependencies. These retain their own licenses;
their versioned license texts are included in the kit's `licenses/` directory.
In particular, Frida's distributed Python extension declares the wxWindows
Library Licence 3.1. Kit source scripts are included under `source/`; the native
Frida extension remains a separate replaceable file in `runtime/`.
The Python distribution also supplies Microsoft Visual C++ runtime DLLs;
its bundled license file includes the applicable third-party notices.
