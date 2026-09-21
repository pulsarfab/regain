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
if sys.argv[1:]:
    raise SystemExit('usage: package-rust.py')
host = next(line.removeprefix("host: ") for line in output("rustc", "-vV").splitlines()
            if line.startswith("host: "))
metadata = json.loads(output("cargo", "metadata", "--locked", "--format-version", "1",
                             "--filter-platform", host))
nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
resolved = set()
pending = list(metadata["workspace_members"])
while pending:
    node = pending.pop()
    if node not in resolved:
        resolved.add(node)
        pending.extend(dependency["pkg"] for dependency in nodes[node]["deps"])
prefix = 'regain-rust'
archive = root / f'{prefix}-{host}.tar.gz'
with tempfile.TemporaryDirectory(prefix="regain-package-") as temporary:
    stage = Path(temporary) / prefix
    licenses = stage / "licenses"
    licenses.mkdir(parents=True)
    for name in ["regain-device", "regain-alpaca", "regain-camera"]:
        binary = name + ('.exe' if 'windows' in host else '')
        shutil.copy2(root / "target" / "release" / binary, stage / binary)
    subprocess.run([sys.executable, str(root / "scripts/stage-sdk.py"), str(stage),
                    "--target", host, "--check"], check=True)
    for name in ["LICENSE", "THIRD_PARTY_NOTICES.md", "Cargo.lock"]:
        shutil.copy2(root / name, stage / name)
    shutil.copy2(root / "docs/portable-rust.md", stage / "README.md")
    shutil.copy2(root / "docs/caa.md", stage / "caa.md")
    shutil.copy2(root / "crates/regain-zwo/LICENSE-ZWO", licenses / "ZWO-CAA-table.txt")
    shutil.copy2(root / "docs/architecture.md", stage / "architecture.md")
    shutil.copy2(root / "docs/ascom.md", stage / "ascom.md")
    shutil.copy2(root / "docs/accessories.md", stage / "accessories.md")
    shutil.copy2(root / "docs/focusers.md", stage / "focusers.md")
    shutil.copy2(root / "docs/ofp2.md", stage / "ofp2.md")
    shutil.copy2(root / "docs/eta.md", stage / "eta.md")
    shutil.copy2(root / "docs/eta-evidence.json", stage / "eta-evidence.json")
    shutil.copy2(root / "docs/eta-serial.jsonl", stage / "eta-serial.jsonl")
    for name in ("focuscube3.md", "focuscube3-evidence.json", "focuscube3-serial.jsonl"):
        shutil.copy2(root / "docs" / name, stage / name)
    shutil.copy2(root / "docs/ofp2-evidence.json", stage / "ofp2-evidence.json")
    (stage / "images").mkdir()
    shutil.copy2(root / "docs/images/native-eta.png", stage / "images/native-eta.png")
    shutil.copy2(root / "docs/images/alpaca-ofp2.png", stage / "images/alpaca-ofp2.png")
    for name in ("alpaca-fc3.png", "native-fc3.png"):
        shutil.copy2(root / "docs/images" / name, stage / "images" / name)
    shutil.copy2(root / "docs/accessory-evidence.json", stage / "accessory-evidence.json")
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
