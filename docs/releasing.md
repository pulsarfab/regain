# CI, releases and NINA distribution

## Build and license

GitHub's **Build and test** workflow runs on pushes, pull requests and manual
dispatch. It runs Rust formatting/lints/tests, the .NET recovery tests, NINA
contract/logo tests, package validation and registry publication fixture tests.
It uploads the ZIP, SHA-256 file, PNG and NINA manifest. No camera is required
on the runner; physical camera and interactive NINA checks remain local.
It also tests all four COM slots from 32-bit and 64-bit clients, checks machine
registration on the disposable Windows runner, and uploads a separate ASCOM
package. Linux and macOS artifacts include the Rust Alpaca server and workers.

PulsarFab regain code and the original logo are Apache-2.0. `LICENSE` contains the full
license; Cargo and .NET metadata declare it. Packages include that license and
third-party notices/licenses. The vendor ASI SDK retains its bundled license.

The source for the logo is `assets/regain.svg`; the 256 × 256 PNG is embedded
as a WPF resource. `FeaturedImageURL` uses a pack URI for installed plugins,
so the logo does not depend on GitHub access. Registry manifests instead use
the versioned PNG release asset for users browsing available plugins.
The PNG is rendered by `scripts/render-brand.cjs` using `sharp` from the SVG;
it is checked in, so builds need no image renderer or Node dependency.

## Version and draft release

`Directory.Build.props` is the four-part .NET/NINA version source of truth.
The first three components must match the Rust workspace version in
`Cargo.toml`; the fourth permits plugin-only revisions. The regain entries in
`[workspace.dependencies]` repeat that Rust version, because crates.io needs a
version on each path dependency. `scripts/version.ps1` rejects a mismatched
tag, Rust version or dependency version. Because a crates.io version cannot be
reused, a plugin-only revision (for example `0.5.1.1`) publishes no crates. Both packaged .NET assemblies and
the camera's displayed driver version use the same version.

1. Update the version in `Directory.Build.props`, `[workspace.package]` and
   `[workspace.dependencies]`, run `cargo check` to refresh `Cargo.lock`, and
   replace `docs/release-notes.md`. Merge to main.
2. Run `scripts/test.ps1`, `scripts/build.ps1`, and `scripts/test-release.ps1`.
   A manual **Release** workflow run on `main` does the same build and validation
   and uploads artifacts without creating a tag or GitHub release.
3. Push a matching tag, such as `v0.5.10.0`. The **Release** workflow reruns all
   checks and creates a **draft** GitHub release with ten assets:
   `Regain-0.5.10.0.zip`, `Regain-0.5.10.0.manifest.json`, `regain.png`,
   `SHA256SUMS`, `Regain-CameraKit-0.5.10.0-win-x64.zip`, its `.zip.sha256`,
   `Regain-ASCOM-0.5.10.0-win-x64.zip`, its `.zip.sha256`,
   `Regain-ASCOM-0.5.10.0-win-x64-setup.exe`, and its `.exe.sha256`.
4. Inspect/test those artifacts, then publish the draft as a stable release.
   A rerun can refresh a draft, but refuses to overwrite published assets.

5. Dispatch **Publish to NINA registry** on `main` with the published tag.
   It updates the shared registry for both public feeds; release publication
   alone does not dispatch it. Verify the deployed manifests report the new version.
6. Publish the crates from the tagged commit, as described in
   [crates.io publication](#cratesio-publication).

The manifest generator uses the compiled plugin's identity, version, author,
license, minimum NINA version and descriptions. It validates the actual ZIP
layout/assembly versions and computes the checksum from the finished archive.
NINA's own manifest schema validates the result. A four-part version is not
silently rewritten to a three-part semantic version.

Local builds and the ordinary **Build and test** workflow produce unsigned
binaries. The **Release** workflow stages the package, authenticates to Azure
through OIDC in the `release` environment, and signs `Regain.NINA.dll`,
`Regain.Core.dll`, `Regain.Rotator.dll`, `Regain.ASCOM.dll`,
`Regain.ASCOM.Register.exe`, the serial ASCOM local servers,
`regain-device.exe`, `regain-alpaca.exe`, and `regain-camera.exe` with Azure Trusted
Signing. It requires valid signatures from StackFoundry LLC before packaging.
The bundled vendor DLL is left unchanged. ZIP and manifest checksums are
computed after signing.

The ASCOM installer and uninstaller are signed too. CI tests installation,
COM registration, rollback, and removal before creating the draft release.

The same workflow prepares and tests the standalone camera kit, reuses the
signed SDK host, signs `Regain-CameraKit.exe`, verifies both signatures, and
runs a signed-executable smoke test. It then packages the kit and computes its
file/archive checksums. The kit ZIP belongs on GitHub Releases; only the NINA
plugin ZIP is referenced by the plugin registry.

The workflow reads `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`,
`AZURE_SUBSCRIPTION_ID`, `SIGNING_ENDPOINT`, `SIGNING_ACCOUNT` and
`SIGNING_PROFILE` from GitHub variables. These repository variables are present
as of 2026-09-14. The Azure federated credential must match the workflow's
`release` environment; variables alone do not verify the signing service.
After the PulsarFab transfer, the subject is
`repo:pulsarfab@279567456/regain@1369114153:environment:release`.
Run the signing smoke test after changes to the repository name or owner.

## crates.io publication

All nine workspace crates are published to crates.io under the same Rust
version: `regain-worker`, `regain-transport`, `regain-core`, `regain-zwo`,
`regain-pegasus`, `regain-deepskydad`, `regain-wanderer`, `regain-device` and
`regain-alpaca`. Each crate has its own `README.md` and `LICENSE`, and inherits
`version`, `edition`, `rust-version`, `authors`, `repository` and `homepage`
from the workspace. `regain-zwo` is `Apache-2.0 AND MIT` because of the ZWO
notice in `LICENSE-ZWO`. Keep the per-crate README text self-contained and use
absolute links; crates.io cannot resolve links into `docs/`.

The **crates.io packages** job in **Build and test** builds the workspace with
the declared minimum Rust version (1.89) and runs `cargo package --workspace`,
which packages and builds every crate against the others as crates.io would see
them. Raise `rust-version` and that job together.

Publish after the tag exists and its **Release** run has passed. From a clean
checkout of the tag, with a crates.io token that can publish these crates
(`cargo login`, or `CARGO_REGISTRY_TOKEN`):

```sh
git checkout v0.5.10.0
cargo publish --workspace --locked --dry-run
cargo publish --workspace --locked
```

`cargo publish --workspace` (Cargo 1.90 or later) uploads the crates in
dependency order and waits for each to appear in the index. If it stops
partway, rerun `cargo publish -p <crate>` for the remaining crates in the order
listed above. A published version cannot be replaced; fix a bad release with a
new version and `cargo yank` the bad one.

## Registry publication

The target is `theatrus/nina-plugins-registry`, deployed to
both `https://nina-plugins.pulsarfab.com/` and
`https://nina-plugins.psf-guard.com/`. They share the same files and deployment;
publish once to update both NINA sources. It expects:

```text
manifests/z/ZwoGain/3.2.0.9001/manifest.json
```

Pushing that repository's `main` triggers its existing deployment. This
workflow changes only the version manifest, not the registry's landing page.

Configure the PulsarFab regain repository secret `NINA_REGISTRY_TOKEN` with a fine-grained
token that has Contents read/write access to `theatrus/nina-plugins-registry`.
The source repository's `GITHUB_TOKEN` cannot write to the other repository.
The publication job uses the secret only for the registry checkout and push;
release lookup uses the source repository token.

Run **Publish to NINA registry** from `main`, supplying a published stable tag.
It checks identity/version, rejects draft/prerelease/channel manifests,
downloads the plugin ZIP and logo **without authentication**, verifies the ZIP
SHA-256 and logo consistency, prevents downgrades/version replacement, and only
then commits and pushes the manifest. Repeating the exact same publication is
a no-op. Concurrent registry changes cause a normal push rejection, not a
force push.

The same operation can be prepared locally using existing `gh`/git credentials:

```powershell
./scripts/publish-registry.ps1 -Tag v0.5.10.0 -RegistryPath ../nina-plugins-registry
# Review the resulting diff. To have the script commit/push, use a clean checkout
# and invoke it with -Push instead of the preparation-only invocation.
```

`scripts/test-release.ps1` exercises the real publication script using a local
fixture repository and mocked downloads. It validates a good release and
rejects inaccessible assets, drafts, checksum corruption and version mismatch;
it never pushes or makes HTTP requests.

## Publication credentials

PulsarFab regain is public. `NINA_REGISTRY_TOKEN` is required for the cross-repository
GitHub workflow; it is not needed when the local publisher uses existing
`gh` and git credentials with registry write access. In either case the
publisher checks anonymous access to the actual ZIP and logo before writing
the registry entry.

No public release or registry entry is created by ordinary pushes to main.
