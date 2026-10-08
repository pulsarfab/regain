# Windows runtime dependency

Regain's Windows Rust workers statically link the MSVC C runtime. The NINA ZIP,
ASCOM ZIP and ASCOM installer do not require users to install the Visual C++
Redistributable separately. Windows 10 or later, the frontend's existing
NINA/.NET/ASCOM requirements, and applicable USB device drivers still apply.

## Investigation

Inspection of existing release executables found `VCRUNTIME140.dll` imports in
`regain-device.exe`, `regain-camera.exe` and `regain-alpaca.exe`. All three also
imported Universal CRT API sets. The bundled `ASICamera2.dll` imports only Windows
system DLLs and does not introduce a VC runtime dependency. Copying files into
the installer alone would leave portable plugin/ASCOM ZIP users exposed.

The repository now uses `target-feature=+crt-static` for Windows MSVC targets in
`.cargo/config.toml`, covering local, CI and release builds. Unix targets retain
their existing linkage. The rebuilt workers import only Windows system DLLs;
the vendor SDK is unchanged and remains a separately loaded library.

This uses Rust's supported [static CRT linkage](https://doc.rust-lang.org/reference/linkage.html#static-and-dynamic-c-runtimes).
Microsoft documents the corresponding [static runtime libraries](https://learn.microsoft.com/en-us/cpp/c-runtime-library/crt-library-features?view=msvc-170).
Source consumers who override Cargo's target flags must retain `+crt-static` to
produce the same standalone Windows executables. Crates embedded in another
application follow that application's linking configuration.

## Packaging guard

`scripts/check-windows-runtime.py` reads PE files without loading them. It checks
ordinary and delayed imports in x86/x64 headers, traverses every staged EXE/DLL,
and rejects versioned VCRUNTIME, MSVCP, MSVCR, CONCRT and VCOMP dependencies.
Malformed files, missing stages and empty stages fail closed. Windows-provided
`msvcrt.dll`, UCRT and API-set DLLs are not mistaken for redistributables.

Both staging and `-PackageOnly` paths run the guard; the installer also validates
its input. This detects accidental dynamic linking despite a developer machine
or CI runner already having the runtime installed. The separate camera exercise
kit still bundles Python/Frida dependencies; its Rust worker no longer depends on
the Python installation supplying a runtime DLL.

```powershell
python -m unittest discover -s scripts -p test_windows_runtime.py
python scripts/check-windows-runtime.py artifacts/stage artifacts/ascom-stage
```

Regression cases cover both PE formats, delayed imports including legacy virtual
addresses, nested vendor dependencies, malformed/truncated structures and the
real bundled SDK. Local release builds, simulated accessory/camera/Alpaca checks
and packaging evidence are recorded with the fix's pull requests. No attached
hardware or installed vendor driver is opened for this investigation.
