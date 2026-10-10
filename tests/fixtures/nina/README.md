# NINA image-save test dependency

`NOVAS31lib.dll` is the x64 NOVAS C3.1 library distributed by NINA. NINA's
ordinary image filename formatter calculates Modified Julian Date even when
the chosen filename contains no date token. The NINA.Plugin NuGet package
does not contain this native dependency. The private FITS-save fixtures use
the real NINA image writer, so the test output needs this library.

Source: [NINA external dependencies, pinned commit](https://github.com/isbeorn/nina.external/blob/07722c82008d6111ed8df71393e8470a8b3025d6/x64/NOVAS/NOVAS31lib.dll).
That dependency revision is referenced by NINA commit
`81582521b7a08cd68fcf2ca5cf9d8d9b195c65da`.

SHA-256: `829859db52ce15a85454e7b76d3fc387529386ed115e17aa4ace228783deb510`.
Size: 187392 bytes. The copy is test-only and is not included in Regain payloads.
No installed NINA or vendor-driver paths are searched by the tests.

NOVAS is provided by the Astronomical Applications Department of the U.S.
Naval Observatory. Its upstream README states that it has no licensing
requirements. The complete README is retained in `NOVAS-README.txt`, from
[NINA's NOVAS source](https://github.com/isbeorn/nina/blob/81582521b7a08cd68fcf2ca5cf9d8d9b195c65da/NOVAS31/NOVAS31/README.txt).
