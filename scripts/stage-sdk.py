"""Stage the matching native SDK and check that the real library loads."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess


def output(*args):
    return subprocess.check_output(args, text=True).strip()


def stage(destination, target):
    root = Path(__file__).resolve().parent.parent
    source = root / "vendor/zwo/native"
    manifest = json.loads((source / "sdk.json").read_text())
    entry = manifest["libraries"][target]
    library = source / entry["file"]
    assert hashlib.sha256(library.read_bytes()).hexdigest() == entry["sha256"], "Vendor SDK checksum mismatch"
    destination.mkdir(parents=True, exist_ok=True)
    installed = destination / library.name
    shutil.copy2(library, installed)
    installed.chmod(0o755)
    licenses = destination / "licenses"
    licenses.mkdir(exist_ok=True)
    shutil.copy2(source / "LICENSE.txt", licenses / "ZWO-ASI-SDK.txt")
    shutil.copy2(source / "sdk.json", destination / "sdk-source.json")
    runtime = {}
    if target.endswith("apple-darwin"):
        # Intel's SDK already uses @loader_path. Relocate ARM's hard-coded
        # Homebrew dependency in the staged copy, preserving vendor originals.
        prefix = Path(output("brew", "--prefix", "libusb"))
        usb = prefix / "lib/libusb-1.0.0.dylib"
        assert usb.is_file(), "Install libusb with Homebrew first"
        shutil.copy2(usb, destination / usb.name)
        (destination / usb.name).chmod(0o755)
        texts = [p for p in prefix.iterdir() if p.is_file()
                 and p.name.upper().startswith(("COPYING", "LICENSE"))]
        assert texts, "Missing libusb license text in Homebrew package"
        for text in texts:
            shutil.copy2(text, licenses / ("libusb-" + text.name))
        formula = json.loads(output("brew", "info", "--json=v2", "libusb"))["formulae"][0]
        runtime = dict(name="libusb", version=formula["versions"]["stable"],
                       license=formula["license"], source=formula["urls"]["stable"]["url"])
        if target.startswith("aarch64"):
            subprocess.run(["install_name_tool", "-change",
                            "/opt/homebrew/opt/libusb/lib/libusb-1.0.0.dylib",
                            "@loader_path/libusb-1.0.0.dylib", str(installed)], check=True)
        for path in [destination / usb.name, installed]:
            subprocess.run(["codesign", "--force", "--sign", "-", str(path)], check=True)
        runtime["sha256"] = hashlib.sha256((destination / usb.name).read_bytes()).hexdigest()
        (destination / "libusb-source.json").write_text(json.dumps(runtime, indent=2) + "\n")
    report = dict(version=manifest["version"], target=target, library=installed.name,
                  sourceSha256=entry["sha256"], stagedSha256=hashlib.sha256(installed.read_bytes()).hexdigest(),
                  runtime=runtime)
    (destination / "sdk-build.json").write_text(json.dumps(report, indent=2) + "\n")
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--target")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    target = args.target or next(line.removeprefix("host: ") for line in output("rustc", "-vV").splitlines()
                                 if line.startswith("host: "))
    destination = args.destination.resolve()
    report = stage(destination, target)
    if args.check:
        result = json.loads(subprocess.check_output([str(destination / "regain-device"), "zwo", "camera-sdk", "--list"],
                                                   text=True, timeout=30))
        assert result["sdkVersion"].replace(" ", "").startswith("1,41,"), result
        assert isinstance(result["cameras"], list), result
        report["sdkLoadCheck"] = result
    print(json.dumps(report, indent=2))
