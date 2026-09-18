"""Package native Rust workers and their licenses for hardware testing."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile


def output(*args):
    return subprocess.check_output(args, text=True).strip()


root = Path(__file__).resolve().parent.parent
caa_only = sys.argv[1:] == ['--caa-only']
if sys.argv[1:] and not caa_only:
    raise SystemExit('usage: package-rust.py [--caa-only]')
host = next(line.removeprefix("host: ") for line in output("rustc", "-vV").splitlines()
            if line.startswith("host: "))
metadata = json.loads(output("cargo", "metadata", "--locked", "--format-version", "1",
                             "--filter-platform", host))
nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
resolved = set()
pending = [p['id'] for p in metadata['packages'] if p['name'] == 'zwogain-caa'] if caa_only else list(metadata["workspace_members"])
while pending:
    node = pending.pop()
    if node not in resolved:
        resolved.add(node)
        pending.extend(dependency["pkg"] for dependency in nodes[node]["deps"])
prefix = 'zwogain-caa' if caa_only else 'zwogain-rust'
archive = root / (f'{prefix}-{host}.zip' if caa_only else f'{prefix}-{host}.tar.gz')
with tempfile.TemporaryDirectory(prefix="zwogain-package-") as temporary:
    stage = Path(temporary) / prefix
    licenses = stage / "licenses"
    licenses.mkdir(parents=True)
    for name in ['zwogain-caa'] if caa_only else ["zwogain-direct", "zwogain-host", "zwogain-alpaca", "zwogain-caa"]:
        binary = name + ('.exe' if 'windows' in host else '')
        shutil.copy2(root / "target" / "release" / binary, stage / binary)
    if not caa_only:
        subprocess.run([sys.executable, str(root / "scripts/stage-sdk.py"), str(stage),
                        "--target", host, "--check"], check=True)
    for name in ["LICENSE", "THIRD_PARTY_NOTICES.md", "Cargo.lock"]:
        shutil.copy2(root / name, stage / name)
    shutil.copy2(root / ("docs/caa.md" if caa_only else "docs/portable-rust.md"), stage / "README.md")
    shutil.copy2(root / "docs/caa.md", stage / "caa.md")
    shutil.copy2(root / "crates/zwogain-caa/LICENSE-ZWO", licenses / "ZWO-CAA-table.txt")
    if not caa_only:
        shutil.copy2(root / "docs/architecture.md", stage / "architecture.md")
        shutil.copy2(root / "docs/ascom.md", stage / "ascom.md")
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
    if caa_only:
        with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED) as bundle:
            for path in sorted(stage.rglob('*')):
                if path.is_file(): bundle.write(path, path.relative_to(stage.parent))
    else:
        with tarfile.open(archive, "w:gz") as tar:
            tar.add(stage, arcname=stage.name)
digest = hashlib.sha256(archive.read_bytes()).hexdigest()
archive.with_suffix(archive.suffix + ".sha256").write_text(f"{digest}  {archive.name}\n")
print(archive)
