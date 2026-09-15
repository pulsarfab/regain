"""Package native Rust workers and their licenses for hardware testing."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile


def output(*args):
    return subprocess.check_output(args, text=True).strip()


root = Path(__file__).resolve().parent.parent
host = next(line.removeprefix("host: ") for line in output("rustc", "-vV").splitlines()
            if line.startswith("host: "))
metadata = json.loads(output("cargo", "metadata", "--locked", "--format-version", "1",
                             "--filter-platform", host))
resolved = {node["id"] for node in metadata["resolve"]["nodes"]}
archive = root / f"zwogain-rust-{host}.tar.gz"
with tempfile.TemporaryDirectory(prefix="zwogain-package-") as temporary:
    stage = Path(temporary) / "zwogain-rust"
    licenses = stage / "licenses"
    licenses.mkdir(parents=True)
    for name in ["zwogain-direct", "zwogain-host"]:
        shutil.copy2(root / "target" / "release" / name, stage / name)
    subprocess.run([sys.executable, str(root / "scripts/stage-sdk.py"), str(stage),
                    "--target", host, "--check"], check=True)
    for name in ["LICENSE", "THIRD_PARTY_NOTICES.md", "Cargo.lock"]:
        shutil.copy2(root / name, stage / name)
    shutil.copy2(root / "docs/portable-rust.md", stage / "README.md")
    for package in metadata["packages"]:
        if package["id"] not in resolved or package["source"] is None:
            continue
        source = Path(package["manifest_path"]).parent
        texts = [p for p in source.iterdir() if p.is_file()
                 and p.name.upper().startswith(("LICENSE", "COPYING", "NOTICE", "COPYRIGHT"))]
        assert texts, f"Missing license text: {package['name']}"
        destination = licenses / f"{package['name']}-{package['version']}"
        destination.mkdir()
        for text in texts:
            shutil.copy2(text, destination / text.name)
    sysroot = Path(output("rustc", "--print", "sysroot"))
    shutil.copy2(sysroot / "share/doc/rust/COPYRIGHT-library.html",
                 licenses / "Rust-Standard-Library.html")
    (stage / "build.json").write_text(json.dumps(dict(
        target=host, commit=output("git", "rev-parse", "HEAD"),
        rustc=output("rustc", "--version")), indent=2) + "\n")
    with tarfile.open(archive, "w:gz") as tar:
        tar.add(stage, arcname=stage.name)
digest = hashlib.sha256(archive.read_bytes()).hexdigest()
archive.with_suffix(archive.suffix + ".sha256").write_text(f"{digest}  {archive.name}\n")
print(archive)
