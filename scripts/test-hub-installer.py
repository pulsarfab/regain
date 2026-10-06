"""Dynamic hub inventory through the real installer; disposable Windows CI only."""
import argparse
import ctypes
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


def paths(binding):
    kind = binding["deviceType"]
    name = f'https://pulsarfab.com/regain/ascom-hub/output/{binding["instanceId"]}/{binding["outputId"]}/{kind}'
    clsid = uuid.uuid5(uuid.NAMESPACE_URL, name)
    progid = "Rgn.H" + dict(switch="S", safetymonitor="M", observingconditions="W")[kind] + "." + clsid.hex
    return (f'Software\\Classes\\CLSID\\{{{clsid}}}', f'Software\\Classes\\{progid}',
            f'Software\\Classes\\AppID\\{{{clsid}}}',
            f'Software\\ASCOM\\{dict(switch="Switch", safetymonitor="SafetyMonitor", observingconditions="ObservingConditions")[kind]} Drivers\\{progid}',
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
                            assert winreg.QueryValueEx(key, "InstallDirectory")[0] == state["install"]
                            assert winreg.QueryValueEx(key, "OwnerSid")[0] == state["owner"]
                            assert winreg.QueryValueEx(key, "Phase")[0] == "ready"
                except FileNotFoundError:
                    assert not present, "Upgrade or failed uninstall removed a dynamic registration"
        if present:
            for view in VIEWS:
                with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, paths(entry["binding"])[0] + r"\LocalServer32", 0, winreg.KEY_READ | view) as key:
                    assert winreg.QueryValueEx(key, "")[0] == command(Path(state["install"]), Path(state["bindings"]), state["owner"])


def prepare_state(install, folder, owner, count):
    folder.mkdir(parents=True, exist_ok=False)
    config = json.loads((ROOT / "crates/regain-hub/examples/simulated-observatory.json").read_text())
    config["instanceId"], config["revision"] = str(uuid.uuid4()), str(uuid.uuid4())
    config_path, bindings_path = folder / "hub.json", folder / "bindings.json"
    config_path.write_text(json.dumps(config), encoding="utf-8")
    bindings = []
    for output in config["outputs"][:count]:
        kind = dict(switch="switch", safety="safetymonitor", weather="observingconditions")[output["device"]["kind"]]
        bindings.append(dict(configPath=str(config_path), instanceId=config["instanceId"], outputId=output["id"],
                             deviceType=kind, label=output["label"], simulated=True))
    revision = str(uuid.uuid4())
    bindings_path.write_text(json.dumps(dict(schemaVersion=1, revision=revision, bindings=bindings)), encoding="utf-8")
    return dict(install=str(install), bindings=str(bindings_path), owner=owner, revision=revision,
                entries=[dict(binding=binding) for binding in bindings])


def register(state):
    for entry in state["entries"]:
        binding = entry["binding"]
        subprocess.run([str(Path(state["install"]) / "Regain.ASCOM.Register.exe"), "/hubregister", state["bindings"], state["revision"],
                        binding["instanceId"], binding["outputId"], state["owner"]],
                       check=True, timeout=15, creationflags=subprocess.CREATE_NO_WINDOW)
    assert_entries(state, True)


def main():
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
        states = [prepare_state(install, folder / "Primary", owner, 3), prepare_state(other, folder / "Other", owner, 1)]
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
        elif options.phase in ("break", "repair"):
            entry = primary["entries"][-1]
            path = paths(entry["binding"])[0] + r"\LocalServer32"
            expected = command(install, Path(primary["bindings"]), primary["owner"])
            with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, path, 0, winreg.KEY_READ | winreg.KEY_SET_VALUE | VIEWS[1]) as key:
                actual = winreg.QueryValueEx(key, "")[0]
                assert actual in (expected, expected + " --fixture-conflict"), "Changed entry no longer belongs to this fixture"
                winreg.SetValueEx(key, "", 0, winreg.REG_SZ, expected + (" --fixture-conflict" if options.phase == "break" else ""))
        elif options.phase == "delete-bindings":
            Path(primary["bindings"]).unlink()
        elif options.phase == "removed":
            assert_entries(primary, False)
            assert_entries(other, True)
            assert Path(other["bindings"]).exists(), "Uninstall changed the other install's settings"
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
