# Native ASI SDK 1.41

Libraries imported from `ASI_linux_mac_SDK_V1.41.tar.bz2`, supplied by the user.
`sdk.json` records the archive hash, source entries, and each library's SHA-256.
The archive's header matches `../ASICamera2.h` after line-ending normalization.
Only the four dynamic libraries used by CI are included; static libraries,
32-bit Linux builds, and demos are omitted.

`LICENSE.txt`, `README.vendor.txt`, and `asi.rules.vendor` are copied from the
archive. The vendor rules are reference material, not installed automatically:
they grant access to every user and change the global USB memory limit.
See `docs/portable-rust.md` for a desktop permission rule.

The original libraries here are unchanged. `scripts/stage-sdk.py` verifies
their hashes and copies the matching library beside the worker. macOS packages
also contain Homebrew's libusb, its license and source information. Staging
changes the ARM SDK's libusb dependency to `@loader_path`, then applies ad-hoc
signatures to both libraries. `sdk-build.json` records the staged library hash.
Linux uses the distribution's libusb runtime.
