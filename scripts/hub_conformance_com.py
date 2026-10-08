"""Private, simulation-only native COM publication for the conformance runner.

No Chooser/profile/production inventory registration. Temporary ProgID aliases
only let ConformU's CLI infer a class; all calls use the production CLSIDs/server.
"""
from contextlib import contextmanager
import ctypes
import json
import os
from pathlib import Path
import subprocess
import time
import uuid
import winreg


@contextmanager
def publish(binary, config, directory, architecture):
    if ctypes.windll.shell32.IsUserAnAdmin():
        raise RuntimeError("Private conformance COM registration requires an unelevated user")
    server_exe = binary.parent / "hub-ascom" / architecture / "Regain.Hub.ASCOM.exe"
    if not server_exe.is_file():
        raise FileNotFoundError(f"Build the native hub ASCOM server first: {server_exe}")
    if any(source["backend"]["kind"] not in ("simulated", "native") for source in config["sources"]):
        raise RuntimeError("Conformance publication accepts only the runner's explicit simulations")
    powershell = Path(os.environ["WINDIR"]) / "System32/WindowsPowerShell/v1.0/powershell.exe"
    flags = subprocess.CREATE_NO_WINDOW
    owner = subprocess.check_output([str(powershell), "-NoProfile", "-Command",
        "[Security.Principal.WindowsIdentity]::GetCurrent().User.Value"],
        text=True, timeout=10, creationflags=flags).strip()
    bindings, entries, devices, roots = [], {}, {}, []
    app_id = "{" + str(uuid.uuid4()) + "}"
    ready = directory / f"com-{architecture}-ready"
    saved = directory / f"com-{architecture}-bindings.json"
    # This fixture manually owns the server. If it dies, activation must fail,
    # rather than letting SCM launch an untracked replacement during cleanup.
    disabled_launch = directory / "no-scm-launch.exe"
    assert not disabled_launch.exists()
    for output in config["outputs"]:
        kind = output["device"].get("deviceType") or {
            "switch": "switch", "safety": "safetymonitor", "weather": "observingconditions"
        }[output["device"]["kind"]]
        class_id = uuid.uuid5(uuid.NAMESPACE_URL,
            f'https://pulsarfab.com/regain/ascom-hub/output/{config["instanceId"]}/{output["id"]}/{kind}')
        # The production 39-character ID preserves the whole UUID. ConformU's
        # CLI instead requires a device-class suffix. This random private alias
        # resolves to the same CLSID and is removed with the fixture.
        alias = f"Rgn.F{uuid.uuid4().hex[:12]}.{kind}"
        assert len(alias) <= 39 and kind not in devices
        devices[kind] = alias
        bindings.append(dict(configPath=str(directory / "hub.json"), instanceId=config["instanceId"],
            outputId=output["id"], deviceType=kind, label=output["label"], simulated=True))
        clsid = f"Software\\Classes\\CLSID\\{{{class_id}}}"
        roots.extend((clsid, f"Software\\Classes\\{alias}"))
        entries.update({clsid: "Regain private conformance fixture",
            clsid + "\\LocalServer32": f'"{disabled_launch}"',
            clsid + "\\ProgID": alias,
            f"Software\\Classes\\{alias}\\CLSID": "{" + str(class_id) + "}"})
    saved.write_text(json.dumps(dict(schemaVersion=1, revision=str(uuid.uuid4()), bindings=bindings)), encoding="utf-8")
    roots.append(f"Software\\Classes\\AppID\\{app_id}")
    views = (winreg.KEY_WOW64_32KEY, winreg.KEY_WOW64_64KEY)
    for hive in (winreg.HKEY_CURRENT_USER, winreg.HKEY_LOCAL_MACHINE):
        for view in views:
            for key in roots:
                try:
                    with winreg.OpenKey(hive, key, 0, winreg.KEY_READ | view):
                        raise RuntimeError("Private conformance registration already exists")
                except FileNotFoundError:
                    pass
    created, server = [], None
    try:
        for view in views:
            for key in roots:
                with winreg.CreateKeyEx(winreg.HKEY_CURRENT_USER, key, 0, winreg.KEY_WRITE | view):
                    created.append((view, key))
            for key, value in entries.items():
                with winreg.CreateKeyEx(winreg.HKEY_CURRENT_USER, key, 0, winreg.KEY_WRITE | view) as opened:
                    winreg.SetValueEx(opened, "", 0, winreg.REG_SZ, value)
                    if key.endswith("\\LocalServer32"):
                        winreg.SetValueEx(opened, "ServerExecutable", 0, winreg.REG_SZ, str(disabled_launch))
                    elif key in roots and "\\CLSID\\{" in key:
                        winreg.SetValueEx(opened, "AppID", 0, winreg.REG_SZ, app_id)
        with (directory / f"com-{architecture}-server.log").open("w", encoding="utf-8") as log:
            server = subprocess.Popen([str(server_exe), "--export", "--ready", str(ready),
                "--owner-sid", owner, "--bindings", str(saved), "--host", str(binary)],
                stdout=log, stderr=log, creationflags=flags)
            deadline = time.monotonic() + 20
            while not ready.exists():
                if server.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError(f"Private COM factories did not start; see {log.name}")
                time.sleep(0.025)
            yield dict(devices=devices, serverPath=str(server_exe), bindings=str(saved),
                serverArchitecture=architecture, registrationRoots=roots)
    finally:
        failures = []
        if server is not None:
            try:
                if server.poll() is None:
                    server.terminate()
                try:
                    server.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=5)
            except Exception as error:
                failures.append(f"Owned COM process cleanup failed: {error}")
        delete_tree = ctypes.windll.advapi32.RegDeleteTreeW
        delete_tree.argtypes = (ctypes.c_void_p, ctypes.c_wchar_p)
        delete_tree.restype = ctypes.c_long
        for view, key in reversed(created):
            parent, name = key.rsplit("\\", 1)
            try:
                with winreg.OpenKey(winreg.HKEY_CURRENT_USER, parent, 0, winreg.KEY_ALL_ACCESS | view) as opened:
                    status = delete_tree(int(opened), name)
                    if status not in (0, 2):
                        failures.append(f"Registry cleanup failed: {key}: {status}")
            except FileNotFoundError:
                pass
            except OSError as error:
                failures.append(f"Registry cleanup failed: {key}: {error}")
        for view, key in created:
            try:
                with winreg.OpenKey(winreg.HKEY_CURRENT_USER, key, 0, winreg.KEY_READ | view):
                    failures.append(f"Private registration remains: {key}")
            except FileNotFoundError:
                pass
            except OSError as error:
                failures.append(f"Cannot verify private registration removal: {key}: {error}")
        (directory / f"com-{architecture}-cleanup.json").write_text(json.dumps({
            "registeredRoots": len(created), "serverStopped": server is None or server.poll() is not None,
            "failures": failures}, indent=2), encoding="utf-8")
        if failures:
            raise RuntimeError(f"Private COM registration cleanup failed: {failures}")
