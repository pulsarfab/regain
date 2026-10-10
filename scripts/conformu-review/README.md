# ConformU 4.5 review corrections

This is a proposed test-tool patch, not a Regain driver change or an official
ConformU release. The patch is GPL-3.0, like the upstream ConformU code it changes;
see [LICENSE.txt](LICENSE.txt). It is not linked into or shipped with Regain.
Upstream copyright belongs to the ConformU authors.

The [focuser Move contract](https://ascom-standards.org/newdocs/focuser.html#Focuser.Move)
specifies InvalidValue for an invalid absolute target, and MaxIncrement specifies
the largest single move. The patch traverses endpoints in bounded steps and
checks that rejected targets neither move nor change position. The conflicting
MaxStep note about automatic stopping remains a question for the standards
maintainers. This patch explicitly follows the Move exception contract.

The [camera binning contract](https://ascom-standards.org/newdocs/camera.html#Camera.BinX)
allows unsupported factors. The existing property tests already handle these.
The exposure tests now handle InvalidValue for an intermediate factor that was
not accepted by the property tests. Bin 1, the advertised maximum, previously
accepted factors, and all other exception types still fail normally. Supported
bins still undergo the full exposure, geometry and image tests.

No timing targets, production capabilities or result files are changed. Keep
stock results alongside these explicitly labelled review results. A review pass
does not become an unmodified ConformU pass or resolve upstream agreement.

Build against the exact upstream
[v4.5.0 source ZIP](https://github.com/ASCOMInitiative/ConformU/archive/refs/tags/v4.5.0.zip):

```powershell
python scripts/prepare-conformu-review.py C:/path/to/ConformU-v4.5.0.zip
# Use the executable path printed by the preparer:
python scripts/test-hub-conformance.py --conformu C:/path/to/conformu.exe --validator-kind review --mode interface --native-ascom x64 --classes focuser camera --camera-backend sdk-simulated
```

The preparer verifies the archive and both original source hashes, extracts to
a fresh artifacts directory, applies the patch, builds a version containing
`regain-review`, and records archive, source, patch and assembly hashes. The normal runner defaults
to stock and refuses a mismatched version label. Neither command opens physical hardware.
