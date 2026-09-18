# CI, releases and the theatr.us NINA registry

## Build and license

GitHub's **Build and test** workflow runs on pushes, pull requests and manual
dispatch. It runs Rust formatting/lints/tests, the .NET recovery tests, NINA
contract/logo tests, package validation and registry publication fixture tests.
It uploads the ZIP, SHA-256 file, PNG and NINA manifest. No camera is required
on the runner; physical camera and interactive NINA checks remain local.
It also tests all four COM slots from 32-bit and 64-bit clients, checks machine
registration on the disposable Windows runner, and uploads a separate ASCOM
package. Linux and macOS artifacts include the Rust Alpaca server and workers.

ZWOgain code and the original logo are Apache-2.0. `LICENSE` contains the full
license; Cargo and .NET metadata declare it. Packages include that license and
third-party notices/licenses. The vendor ASI SDK retains its bundled license.

The source for the logo is `assets/zwogain.svg`; the 256 × 256 PNG is embedded
as a WPF resource. `FeaturedImageURL` uses a pack URI for installed plugins,
so the logo does not depend on GitHub access. Registry manifests instead use
the versioned PNG release asset for users browsing available plugins.
The PNG was rendered with `@resvg/resvg-js` 2.6.2 from the SVG; it is checked
in, so builds need no image renderer or Node dependency.

## Version and draft release

`Directory.Build.props` is the four-part .NET/NINA version source of truth.
The first three components must match the Rust workspace version in
`Cargo.toml`; the fourth permits plugin-only revisions. `scripts/version.ps1`
rejects a mismatched tag or Rust version. Both packaged .NET assemblies and
the camera's displayed driver version use the same version.

1. Update the source version and `docs/release-notes.md`, then merge to main.
2. Run `scripts/test.ps1`, `scripts/build.ps1`, and `scripts/test-release.ps1`.
   A manual **Release** workflow run on `main` does the same build and validation
   and uploads artifacts without creating a tag or GitHub release.
3. Push a matching tag, such as `v0.3.0.0`. The **Release** workflow reruns all
   checks and creates a **draft** GitHub release with ten assets:
   `ZwoGain-0.3.0.0.zip`, `ZwoGain-0.3.0.0.manifest.json`, `zwogain.png`,
   `SHA256SUMS`, `ZwoGain-CameraKit-0.3.0.0-win-x64.zip`, its `.zip.sha256`,
   `ZwoGain-ASCOM-0.3.0.0-win-x64.zip`, its `.zip.sha256`,
   `ZwoGain-ASCOM-0.3.0.0-win-x64-setup.exe`, and its `.exe.sha256`.
4. Inspect/test those artifacts, then publish the draft as a stable release.
   A rerun can refresh a draft, but refuses to overwrite published assets.

The manifest generator uses the compiled plugin's identity, version, author,
license, minimum NINA version and descriptions. It validates the actual ZIP
layout/assembly versions and computes the checksum from the finished archive.
NINA's own manifest schema validates the result. A four-part version is not
silently rewritten to a three-part semantic version.

Local builds and the ordinary **Build and test** workflow produce unsigned
binaries. The **Release** workflow stages the package, authenticates to Azure
through OIDC in the `release` environment, and signs `ZwoGain.NINA.dll`,
`ZwoGain.Core.dll`, `ZwoGain.Rotator.dll`, `ZwoGain.ASCOM.dll`,
`ZwoGain.ASCOM.Register.exe`, `zwogain-caa.exe`, `zwogain-alpaca.exe`,
`zwogain-host.exe`, and `zwogain-direct.exe` with Azure Trusted
Signing. It requires valid signatures from StackFoundry LLC before packaging.
The bundled vendor DLL is left unchanged. ZIP and manifest checksums are
computed after signing.

The ASCOM installer and uninstaller are signed too. CI tests installation,
COM registration, rollback, and removal before creating the draft release.

The same workflow prepares and tests the standalone camera kit, reuses the
signed SDK host, signs `ZwoGain-CameraKit.exe`, verifies both signatures, and
runs a signed-executable smoke test. It then packages the kit and computes its
file/archive checksums. The kit ZIP belongs on GitHub Releases; only the NINA
plugin ZIP is referenced by the plugin registry.

The workflow reads `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`,
`AZURE_SUBSCRIPTION_ID`, `SIGNING_ENDPOINT`, `SIGNING_ACCOUNT` and
`SIGNING_PROFILE` from GitHub variables. These repository variables are present
as of 2026-09-14. The Azure federated credential must match the workflow's
`release` environment; variables alone do not verify the signing service.

## Registry publication

The target is `theatrus/nina-plugins-registry`, deployed to
`https://nina-plugins.psf-guard.com/`. It expects:

```text
manifests/z/ZwoGain/3.2.0.9001/manifest.json
```

Pushing that repository's `main` triggers its existing deployment. This
workflow changes only the version manifest, not the registry's landing page.

Configure the ZWOgain repository secret `NINA_REGISTRY_TOKEN` with a fine-grained
token that has Contents read/write access to `theatrus/nina-plugins-registry`.
The source repository's `GITHUB_TOKEN` cannot write to the other repository.
The publication job uses the secret only for the registry checkout and push;
release lookup uses the source repository token.

Run **Publish to NINA registry** from `main`, supplying a published stable tag.
It checks identity/version, rejects draft/prerelease/channel manifests,
downloads the installer and logo **without authentication**, verifies the ZIP
SHA-256 and logo consistency, prevents downgrades/version replacement, and only
then commits and pushes the manifest. Repeating the exact same publication is
a no-op. Concurrent registry changes cause a normal push rejection, not a
force push.

The same operation can be prepared locally using existing `gh`/git credentials:

```powershell
./scripts/publish-registry.ps1 -Tag v0.1.0.0 -RegistryPath ../nina-plugins-registry
# Review the resulting diff. To have the script commit/push, use a clean checkout
# and invoke it with -Push instead of the preparation-only invocation.
```

`scripts/test-release.ps1` exercises the real publication script using a local
fixture repository and mocked downloads. It validates a good release and
rejects inaccessible assets, drafts, checksum corruption and version mismatch;
it never pushes or makes HTTP requests.

## Publication credentials

ZWOgain is public. `NINA_REGISTRY_TOKEN` is required for the cross-repository
GitHub workflow; it is not needed when the local publisher uses existing
`gh` and git credentials with registry write access. In either case the
publisher checks anonymous access to the actual ZIP and logo before writing
the registry entry.

No public release or registry entry is created by ordinary pushes to main.
