# Third-party notices

ZWOgain is copyright 2026 Yann Ramin, Apache License 2.0 (see `LICENSE`).
The original camera/recovery logo in `assets/` and the embedded PNG are covered
by the same license.

The Rust C ABI declarations in `crates/zwogain-host/src/raw.rs` were adapted
from [AutoPierCam](https://github.com/theatrus/autopiercam), copyright 2026 Yann
Ramin, Apache License 2.0. AutoPierCam also informed the package and process
architecture.

The ZWO ASI SDK DLL and header are copyright 2015 ZWO Company and distributed
under the MIT-style license in `vendor/zwo/LICENSE.txt`, included in packages
as `licenses/ZWO-ASI-SDK.txt`. ZWO's camera device driver is not included.

Rust dependencies retain their original licenses. The build script packages
their license/copyright texts and the Rust standard-library copyright bundle
under `licenses/`. Cargo.lock records exact versions.

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
