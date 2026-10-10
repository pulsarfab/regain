"""Build an explicitly labelled, isolated review of the ConformU 4.5 test fixes.

Requires the upstream v4.5.0 source ZIP and .NET 10 SDK.
Never changes the supplied archive or the installed validator.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import uuid
import zipfile

ROOT = Path(__file__).resolve().parents[1]
ARCHIVE_SHA256 = "c4f92945dce466c4db110a87a0063e9631466f2150f18af94ec649c53bce6e47"
SOURCES = {
    "ConformU/Conform/FocuserTester.cs": "7bb0442a8cf114b9a026c4d195f2857bf422b21f382cf0f64d557649e2987063",
    "ConformU/Conform/CameraTester.cs": "a5740c0515605492a92613b3b5041f618f3b5bfa69795f7e66b100fb7c1567ce",
}


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    args = parser.parse_args()
    archive = args.archive.resolve(strict=True)
    if sha(archive) != ARCHIVE_SHA256:
        parser.error("Not the pinned upstream v4.5.0 source ZIP")
    folder = ROOT / "artifacts" / ("conformu-review-" + uuid.uuid4().hex)
    folder.mkdir()
    with zipfile.ZipFile(archive) as zipped:
        for name in zipped.namelist():
            if not (folder / name).resolve().is_relative_to(folder.resolve()):
                raise RuntimeError("Archive path escaped the private directory")
        zipped.extractall(folder)
    checkout = folder / "ConformU-4.5.0"
    for name, expected in SOURCES.items():
        if sha(checkout / name) != expected:
            parser.error(f"Not the reviewed upstream v4.5.0 source: {name}")
    # The release archive uses CRLF; the review patch uses canonical LF.
    # Normalize only the private copies after verifying original archive hashes.
    for name in SOURCES:
        copied = checkout / name
        copied.write_text(copied.read_text(encoding="utf-8-sig"), encoding="utf-8-sig", newline="\n")
    patch = ROOT / "scripts/conformu-review/conformu-4.5.patch"
    directory = checkout.relative_to(ROOT).as_posix()
    for extra in (["--check"], []):
        subprocess.run(["git", "apply", *extra, "--directory=" + directory, str(patch)],
                       cwd=ROOT, check=True)
    output = checkout / "ConformU/bin/Release/net10.0-windows"
    with (folder / "build.log").open("w", encoding="utf-8") as log:
        subprocess.run(["dotnet", "build", str(checkout / "ConformU/ConformU.csproj"),
                        "-c", "Release", "-f", "net10.0-windows",
                        "-p:ProductPreRelease=regain-review"], cwd=ROOT,
                       stdout=log, stderr=subprocess.STDOUT, check=True)
    tool = output / "conformu.exe"
    version = subprocess.check_output([str(tool), "--version"], text=True).strip()
    if "regain-review" not in version:
        raise RuntimeError("Review build must identify itself")
    manifest = dict(validatorKind="review", version=version, upstreamTag="v4.5.0",
                    archiveSha256=ARCHIVE_SHA256, patchSha256=sha(patch), originalSourceSha256=SOURCES,
                    reviewedSourceSha256={name: sha(checkout / name) for name in SOURCES},
                    toolSha256=sha(tool), toolAssemblySha256=sha(output / "conformu.dll"))
    (output / "regain-review.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    print(tool, flush=True)


if __name__ == "__main__":
    main()
