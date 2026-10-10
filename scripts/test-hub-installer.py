"""Dynamic hub inventory through the real installer; disposable Windows CI only."""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import uuid
import winreg

ROOT = Path(__file__).resolve().parents[1]
VIEWS = (winreg.KEY_WOW64_32KEY, winreg.KEY_WOW64_64KEY)
INVENTORY = r"Software\PulsarFab\Regain\HubExports"
CLASSES = {
    "switch": ("S", "Switch", 3), "safetymonitor": ("M", "SafetyMonitor", 3),
    "observingconditions": ("W", "ObservingConditions", 2), "focuser": ("F", "Focuser", 4),
    "rotator": ("R", "Rotator", 4), "filterwheel": ("L", "FilterWheel", 3),
    "covercalibrator": ("C", "CoverCalibrator", 2), "camera": ("A", "Camera", 4),
}


def paths(binding):
    kind = binding["deviceType"]
    name = f'https://pulsarfab.com/regain/ascom-hub/output/{binding["instanceId"]}/{binding["outputId"]}/{kind}'
    clsid = uuid.uuid5(uuid.NAMESPACE_URL, name)
    prefix, chooser, _ = CLASSES[kind]
    progid = "Rgn.H" + prefix + "." + clsid.hex
    return (f'Software\\Classes\\CLSID\\{{{clsid}}}', f'Software\\Classes\\{progid}',
            f'Software\\Classes\\AppID\\{{{clsid}}}',
            f'Software\\ASCOM\\{chooser} Drivers\\{progid}',
            INVENTORY + "\\{" + str(clsid) + "}")


def command(install, bindings, owner):
    return f'"{install / "hub-ascom/x64/Regain.Hub.ASCOM.exe"}" /Embedding --owner-sid {owner} --bindings "{bindings}" --host "{install / "regain-alpaca.exe"}"'


def assert_entries(state, present):
    for entry in state["entries"]:
        for view in VIEWS:
            for path in paths(entry["binding"]):
                try:
                    with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, path, 0, winreg.KEY_READ | view) as key:
                        assert present, "Uninstall left a dynamic registration"
                        if path.startswith(INVENTORY):
                            binding = entry["binding"]
                            expected = dict(SchemaVersion=1, Phase="ready", InstallDirectory=state["install"],
                                            OwnerSid=state["owner"], BindingsPath=state["bindings"],
                                            InstanceId=binding["instanceId"], OutputId=binding["outputId"],
                                            DeviceType=binding["deviceType"], Label=binding["label"],
                                            Simulated=1, ConfigPath=binding["configPath"])
                            for name, value in expected.items():
                                assert winreg.QueryValueEx(key, name)[0] == value, f"Inventory changed: {name}"
                except FileNotFoundError:
                    assert not present, "Upgrade or failed uninstall removed a dynamic registration"
        if present:
            class_key, prog_key, app_key, _, _ = paths(entry["binding"])
            clsid = "{" + entry["clsid"] + "}"
            progid = prog_key.rsplit("\\", 1)[1]
            checks = ((class_key, "AppID", clsid), (class_key + r"\ProgID", "", progid),
                      (prog_key + r"\CLSID", "", clsid), (app_key, "RunAs", "Interactive User"),
                      (class_key + r"\LocalServer32", "ServerExecutable",
                       str(Path(state["install"]) / "hub-ascom/x64/Regain.Hub.ASCOM.exe")))
            for view in VIEWS:
                for path, name, expected in checks:
                    with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, path, 0, winreg.KEY_READ | view) as key:
                        assert winreg.QueryValueEx(key, name)[0] == expected, f"Registration changed: {path}/{name}"
                with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, paths(entry["binding"])[0] + r"\LocalServer32", 0, winreg.KEY_READ | view) as key:
                    assert winreg.QueryValueEx(key, "")[0] == command(Path(state["install"]), Path(state["bindings"]), state["owner"])


def assert_settings(state, bindings_present=True):
    for path, digest in state["settings"].items():
        if path == state["bindings"] and not bindings_present:
            assert not Path(path).exists(), "Deleted fixture bindings reappeared"
        else:
            assert hashlib.sha256(Path(path).read_bytes()).hexdigest() == digest, f"Setup changed Hub settings: {path}"


def prepare_state(install, folder, owner):
    folder.mkdir(parents=True, exist_ok=False)
    config = json.loads((ROOT / "crates/regain-hub/examples/simulated-observatory.json").read_text(encoding="utf-8"))
    config["instanceId"], config["revision"] = str(uuid.uuid4()), str(uuid.uuid4())
    # These five bindings exercise registration/metadata only. The typed Alpaca
    # sources deliberately have no running upstream; Connect must never occur.
    for kind in list(CLASSES)[3:]:
        source = str(uuid.uuid4())
        backend = (dict(kind="simulated", deviceType=kind) if kind == "camera" else
                   dict(kind="alpaca", baseUrl="http://127.0.0.1:1", deviceType=kind,
                        deviceNumber=0, connectionPolicy="managed"))
        config["sources"].append(dict(id=source, label=f"Metadata-only simulation {kind}", backend=backend))
        config["outputs"].append(dict(id=str(uuid.uuid4()), number=0, label=f"Simulation {kind}",
                                      device=dict(kind="proxy", source=source, deviceType=kind)))
    config_path, bindings_path = folder / "hub.json", folder / "bindings.json"
    config_path.write_text(json.dumps(config), encoding="utf-8")
    bindings = []
    for output in config["outputs"]:
        kind = output["device"].get("deviceType") or dict(switch="switch", safety="safetymonitor", weather="observingconditions")[output["device"]["kind"]]
        bindings.append(dict(configPath=str(config_path), instanceId=config["instanceId"], outputId=output["id"],
                             deviceType=kind, label=output["label"], simulated=True))
    revision = str(uuid.uuid4())
    bindings_path.write_text(json.dumps(dict(schemaVersion=1, revision=revision, bindings=bindings)), encoding="utf-8")
    entries = [dict(binding=binding, clsid=paths(binding)[0].rsplit("\\", 1)[1].strip("{}")) for binding in bindings]
    identities_path = folder / "identities.json"
    identities_path.write_text(json.dumps([dict(clsid=entry["clsid"], version=CLASSES[entry["binding"]["deviceType"]][2])
                                           for entry in entries]), encoding="utf-8")
    return dict(install=str(install), bindings=str(bindings_path), owner=owner, revision=revision,
                entries=entries, settings={str(path): hashlib.sha256(path.read_bytes()).hexdigest()
                                           for path in (config_path, bindings_path, identities_path)})


def register(state):
    for entry in state["entries"]:
        binding = entry["binding"]
        subprocess.run([str(Path(state["install"]) / "Regain.ASCOM.Register.exe"), "/hubregister", state["bindings"], state["revision"],
                        binding["instanceId"], binding["outputId"], state["owner"]],
                       check=True, timeout=15, creationflags=subprocess.CREATE_NO_WINDOW)
    assert_entries(state, True)


def main():
    if not __debug__:
        raise RuntimeError("Installer acceptance requires Python assertions; do not use -O")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("prepare", "assert", "break", "repair", "delete-bindings", "removed", "cleanup"))
    parser.add_argument("--install", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, required=True)
    options = parser.parse_args()
    if not (os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("RUNNER_OS") == "Windows" and ctypes.windll.shell32.IsUserAnAdmin()):
        raise RuntimeError("Hub installer fixtures require an elevated disposable GitHub Windows runner")
    install, folder = options.install.resolve(), options.fixture.resolve()
    allowed = (ROOT / "artifacts/installer-test").resolve()
    if not (install.is_relative_to(allowed) and folder.is_relative_to(allowed)):
        raise RuntimeError("Installer fixture paths must remain inside the installer test directory")
    manifest = folder / "fixture.json"
    if options.phase == "prepare":
        folder.mkdir(exist_ok=False)
        owner = subprocess.check_output([str(Path(os.environ["WINDIR"]) / "System32/WindowsPowerShell/v1.0/powershell.exe"),
                                         "-NoProfile", "-Command", "[Security.Principal.WindowsIdentity]::GetCurrent().User.Value"],
                                        text=True, timeout=10, creationflags=subprocess.CREATE_NO_WINDOW).strip()
        other = folder / "Other install"
        other.mkdir()
        # A real second helper/payload, with its own random identities. No COM
        # object or equipment connection is created by this lifecycle fixture.
        for pattern in ("*.dll", "Regain.ASCOM.Register.exe*", "regain-alpaca.exe"):
            for source in install.glob(pattern):
                shutil.copy2(source, other / source.name)
        shutil.copytree(install / "hub-ascom", other / "hub-ascom")
        states = [prepare_state(install, folder / "Primary", owner), prepare_state(other, folder / "Other", owner)]
        for state in states:
            for entry in state["entries"]:
                for hive in (winreg.HKEY_CURRENT_USER, winreg.HKEY_LOCAL_MACHINE):
                    for view in VIEWS:
                        for path in paths(entry["binding"]):
                            try:
                                with winreg.OpenKey(hive, path, 0, winreg.KEY_READ | view):
                                    raise RuntimeError("Private installer identity already exists")
                            except FileNotFoundError:
                                pass
        # Persist exact cleanup identities only after every collision check.
        manifest.write_text(json.dumps(states), encoding="utf-8")
        for state in states:
            register(state)
    else:
        states = json.loads(manifest.read_text())
        if len(states) != 2:
            raise RuntimeError("Invalid installer fixture manifest")
        primary, other = states
        if primary["install"] != str(install) or other["install"] != str(folder / "Other install"):
            raise RuntimeError("Fixture belongs to another installation")
        if primary["bindings"] != str(folder / "Primary/bindings.json") or other["bindings"] != str(folder / "Other/bindings.json"):
            raise RuntimeError("Fixture binding paths escaped their private directories")
        for state in states:
            if not all(Path(state[key]).resolve().is_relative_to(allowed) for key in ("install", "bindings")):
                raise RuntimeError("Resolved installer fixture path escaped the test directory")
        if options.phase == "assert":
            for state in states:
                assert_entries(state, True)
                assert_settings(state)
        elif options.phase in ("break", "repair"):
            entry = primary["entries"][-1]
            path = paths(entry["binding"])[0] + r"\LocalServer32"
            expected = command(install, Path(primary["bindings"]), primary["owner"])
            with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, path, 0, winreg.KEY_READ | winreg.KEY_SET_VALUE | VIEWS[1]) as key:
                actual = winreg.QueryValueEx(key, "")[0]
                assert actual in (expected, expected + " --fixture-conflict"), "Changed entry no longer belongs to this fixture"
                winreg.SetValueEx(key, "", 0, winreg.REG_SZ, expected + (" --fixture-conflict" if options.phase == "break" else ""))
        elif options.phase == "delete-bindings":
            assert_settings(primary)
            Path(primary["bindings"]).unlink()
        elif options.phase == "removed":
            assert_entries(primary, False)
            assert_entries(other, True)
            assert_settings(primary, bindings_present=False)
            assert_settings(other)
        elif options.phase == "cleanup":
            # Remove only through the owned helper, retaining its collision and
            # critical-command checks. No raw registry deletion fallback.
            for state in states:
                executable = Path(state["install"]) / "Regain.ASCOM.Register.exe"
                if executable.exists():
                    subprocess.run([str(executable), "/hubunregisterall"], check=True, timeout=15,
                                   creationflags=subprocess.CREATE_NO_WINDOW)
                assert_entries(state, False)
    print("Hub installer fixture:", options.phase, "passed", flush=True)


if __name__ == "__main__":
    main()
