# PulsarFab regain

**Regain control of your equipment.**

PulsarFab regain is the new name for ZWOgain. The project now covers cameras,
rotators, filter wheels, focusers, and flat panels from multiple manufacturers.
The repository is [pulsarfab/regain](https://github.com/pulsarfab/regain).
The rename starts with release 0.4.0.0; earlier releases keep their original names.

## Names and assets

Use **PulsarFab regain** for the product and **regain** in prose. Rust packages
and command names use `regain-`. Vendor packages are `regain-zwo`,
`regain-pegasus`, `regain-deepskydad`, and `regain-wanderer`; common packages are
`regain-core`, `regain-transport`, and `regain-worker`. The executables are
`regain-device`, `regain-alpaca`, and `regain-camera`.
.NET assemblies and namespaces use `Regain.*`; the solution is `Regain.slnx`.
Archives use `Regain-<version>`, `Regain-ASCOM-<version>-win-x64`, and
`Regain-CameraKit-<version>-win-x64`.

The icon combines a pulsar with a return arrow, in amber (`#ffb653`), white,
and dark blue. Its editable source is [regain.svg](../assets/regain.svg);
the README uses [regain-wordmark.svg](../assets/regain-wordmark.svg).
`node scripts/render-brand.cjs` regenerates the PNG and multi-size Windows ICO
exports with the `sharp` Node package installed. Generated icons are committed,
so ordinary builds do not require Node or an image editor. The artwork is Apache-2.0.

## Upgrading

Close equipment applications before upgrading. The ASCOM installer keeps its
existing installation identity and uses the existing folder when upgrading.
It checks both old and new process names and removes known obsolete driver binaries.
Fresh installations use `PulsarFab regain ASCOM`. For manual NINA installation,
remove the old `ZwoGain` plugin folder before extracting `Regain`; loading both
copies would create duplicate equipment providers.

The NINA plugin GUID, equipment IDs, ASCOM CLSIDs, and existing `ASCOM.ZWOgain.*`
ProgIDs remain stable so saved host selections keep working. These are compatibility
identifiers; all displayed driver names use PulsarFab regain. Custom actions are
advertised as `Regain.*`, with the previous `ZwoGain.*` spelling accepted as an alias.
The existing NINA catalog path `manifests/z/ZwoGain` is retained for the same
reason; its displayed manifest name and download URLs use the new brand.
Recorded hardware evidence is preserved verbatim, including historical paths.

Native profiles copy automatically from `%LOCALAPPDATA%\ZwoGain` to
`%LOCALAPPDATA%\Regain` when first needed. Existing regain profiles take precedence;
the originals remain untouched. This includes NINA camera/recovery settings and
native ASCOM, CAA, EFW, EAF, and FocusCube3 profiles.
Alpaca copies its default `cameras*.json` profiles, preserving UUIDs and device
selections. On Linux/macOS the equivalent base is `$XDG_CONFIG_HOME` or
`~/.config`. Explicit `--profiles` paths stay exactly as supplied. Existing log
files remain in the old directory; new logs use the new directory.

Update scripts and services to use `regain-*` commands and `REGAIN_*` environment
variables. Native .NET frontends also accept their old `ZWOGAIN_*` overrides when
the new spelling is unset. No forwarding executables are installed under the old
names. Close any manually launched old server before starting its replacement.

Signing certificates retain their actual legal publisher, StackFoundry LLC;
the product publisher is PulsarFab. GitHub release publishing targets the moved
repository. Its Azure federation must authorize the repository's current OIDC
subject and `release` environment; a repository transfer does not update Azure
federated credentials automatically.
