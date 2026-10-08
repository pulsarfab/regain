"""Stage license texts for the resolved target graph, with pinned offline repairs."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
OVERRIDES = ROOT / "vendor/rust-licenses"
PREFIXES = ("LICENSE", "COPYING", "NOTICE", "COPYRIGHT", "UNLICENSE")


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def package_texts(package, overrides=OVERRIDES):
    source = Path(package["manifest_path"]).resolve().parent
    texts = {path.name: path for path in source.iterdir()
             if path.is_file() and path.name.upper().startswith(PREFIXES)}
    declared = package.get("license_file")
    if declared:
        path = (source / declared).resolve()
        if not path.is_relative_to(source) or not path.is_file():
            raise RuntimeError(f"Missing or escaped declared license: {package['name']}")
        texts[str(path.relative_to(source))] = path
    if texts:
        return sorted(texts.values()), "crate"
    repair = overrides / f"{package['name']}-{package['version']}"
    provenance = repair / "PROVENANCE.json"
    if not provenance.is_file():
        raise RuntimeError(f"Missing license text: {package['name']} {package['version']}")
    record = json.loads(provenance.read_text(encoding="utf-8"))
    if any(package.get(key) != value for key, value in record["package"].items()):
        raise RuntimeError(f"License repair package identity changed: {package['name']}")
    vcs = json.loads((source / ".cargo_vcs_info.json").read_text(encoding="utf-8"))
    if vcs != record["vcs"] or digest(source / "Cargo.toml.orig") != record["manifestSha256"]:
        raise RuntimeError(f"License repair source changed: {package['name']}")
    texts = []
    for name, expected in record["files"].items():
        path = repair / name
        if Path(name).name != name or digest(path) != expected["sha256"]:
            raise RuntimeError(f"Pinned license text changed: {package['name']}/{name}")
        texts.append(path)
    if not texts:
        raise RuntimeError(f"Empty license repair: {package['name']}")
    return sorted(texts) + [provenance], "pinned-upstream"


def stage(metadata, destination, overrides=OVERRIDES):
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    resolved, pending = set(), list(metadata["workspace_members"])
    while pending:
        node = pending.pop()
        if node not in resolved:
            resolved.add(node)
            pending.extend(dependency["pkg"] for dependency in nodes[node]["deps"])
    plan = []
    for package in metadata["packages"]:
        if package["id"] not in resolved or package["source"] is None:
            continue
        name = f"{package['name']}-{package['version']}"
        if Path(name).name != name:
            raise RuntimeError("Invalid license destination")
        texts, origin = package_texts(package, overrides)
        base = (Path(package["manifest_path"]).resolve().parent if origin == "crate" else overrides / name).resolve()
        plan.append((name, [(path, path.resolve().relative_to(base).as_posix(), digest(path)) for path in texts], origin))
    # All missing texts and pinned repairs are checked before staging any crate.
    records = []
    for name, texts, origin in plan:
        folder = destination / name
        folder.mkdir(parents=True, exist_ok=True)
        for path, relative, expected in texts:
            target = folder / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, target)
            if digest(target) != expected:
                raise RuntimeError(f"License changed during staging: {name}/{relative}")
        records.append(dict(package=name, origin=origin,
                            files=[dict(name=relative, sha256=expected) for _, relative, expected in texts]))
    destination.mkdir(parents=True, exist_ok=True)
    (destination / "rust-license-manifest.json").write_text(json.dumps(records, indent=2) + "\n", encoding="utf-8")
    return records


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--destination", type=Path, required=True)
    args = parser.parse_args()
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--format-version", "1",
                                                  "--filter-platform", args.target], cwd=ROOT, text=True))
    records = stage(metadata, args.destination)
    print(f"Staged licenses for {len(records)} resolved Rust packages ({args.target})")


if __name__ == "__main__":
    main()
