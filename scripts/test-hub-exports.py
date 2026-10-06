"""Private real COM exports, simulation only; no installed driver activation."""
import copy
import ctypes
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import uuid
import winreg

ROOT = Path(__file__).resolve().parents[1]
NO_WINDOW = subprocess.CREATE_NO_WINDOW


def main():
    workers = Path(os.environ.get("REGAIN_TEST_WORKERS", ROOT / "target/debug")).resolve()
    host_exe = workers / "regain-alpaca.exe"
    with tempfile.TemporaryDirectory(prefix="hub-export-", dir=ROOT / "artifacts") as directory:
        folder = Path(directory)
        config = json.loads((ROOT / "crates/regain-hub/examples/simulated-observatory.json").read_text())
        config["instanceId"], config["revision"] = str(uuid.uuid4()), str(uuid.uuid4())
        additional = copy.deepcopy(config["outputs"][0])
        additional.update(id=str(uuid.uuid4()), number=1, label="Second simulation controls")
        for channel in additional["device"]["channels"]:
            channel["id"] = str(uuid.uuid4())
        config["outputs"].append(additional)
        for source in config["sources"]:
            source["polling"] = dict(pollSeconds=0.1, requestTimeoutSeconds=0.3, attemptsPerCycle=1,
                                     initialBackoffSeconds=0.05, backoffCapSeconds=0.05)
        path = folder / "configuration.json"
        path.write_text(json.dumps(config), encoding="utf-8")
        bindings, identities = [], []
        for output in config["outputs"]:
            kind = dict(switch="switch", safety="safetymonitor", weather="observingconditions")[output["device"]["kind"]]
            name = f'https://pulsarfab.com/regain/ascom-hub/output/{config["instanceId"]}/{output["id"]}/{kind}'
            clsid = uuid.uuid5(uuid.NAMESPACE_URL, name)  # Independent identity derivation.
            progid = "Rgn.H" + dict(switch="S", safetymonitor="M", observingconditions="W")[kind] + "." + clsid.hex
            assert len(progid) == 39
            identities.append(dict(clsid=str(clsid), progid=progid, version=2 if kind == "observingconditions" else 3))
            bindings.append(dict(configPath=str(path), instanceId=config["instanceId"], outputId=output["id"],
                                 deviceType=kind, label=output["label"], simulated=True))
        (folder / "identities.json").write_text(json.dumps(identities))
        saved = folder / "bindings.json"
        saved.write_text(json.dumps(dict(schemaVersion=1, revision=str(uuid.uuid4()), bindings=bindings)))
        elevated = bool(ctypes.windll.shell32.IsUserAnAdmin())
        if elevated and not (os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("RUNNER_OS") == "Windows"):
            raise RuntimeError("Elevated machine fixture registration is permitted only on disposable GitHub runners")
        hive = winreg.HKEY_LOCAL_MACHINE if elevated else winreg.HKEY_CURRENT_USER
        views = (winreg.KEY_WOW64_32KEY, winreg.KEY_WOW64_64KEY)
        client = ROOT / "scripts/test-hub-export-client.ps1"
        for architecture in ("x86", "x64"):
            server_exe = workers / "hub-ascom" / architecture / "Regain.Hub.ASCOM.exe"
            paths = [item for identity in identities for item in
                     (f'Software\\Classes\\CLSID\\{{{identity["clsid"]}}}', f'Software\\Classes\\{identity["progid"]}')]
            for check_hive in (winreg.HKEY_CURRENT_USER, winreg.HKEY_LOCAL_MACHINE):
                for view in views:
                    for key_path in paths:
                        try:
                            with winreg.OpenKey(check_hive, key_path, 0, winreg.KEY_READ | view):
                                raise RuntimeError("Private export fixture registration already exists")
                        except FileNotFoundError:
                            pass
            created, children = [], []
            server = host = None
            ready = folder / "ready"
            try:
                for view in views:
                    for key_path in paths:
                        created.append((view, key_path))
                        with winreg.CreateKeyEx(hive, key_path, 0, winreg.KEY_WRITE | view):
                            pass
                    for identity in identities:
                        entries = {
                            f'Software\\Classes\\CLSID\\{{{identity["clsid"]}}}\\LocalServer32': f'"{server_exe}" /Embedding',
                            f'Software\\Classes\\CLSID\\{{{identity["clsid"]}}}\\ProgID': identity["progid"],
                            f'Software\\Classes\\{identity["progid"]}\\CLSID': "{" + identity["clsid"] + "}",
                        }
                        for key_path, value in entries.items():
                            with winreg.CreateKeyEx(hive, key_path, 0, winreg.KEY_WRITE | view) as key:
                                winreg.SetValueEx(key, "", 0, winreg.REG_SZ, value)
                environment = {**os.environ, "REGAIN_HUB_BINDINGS": str(saved), "REGAIN_HUB_HOST": str(host_exe)}
                server = subprocess.Popen([str(server_exe), "--export", "--ready", str(ready)], env=environment,
                                          creationflags=NO_WINDOW)
                deadline = time.monotonic() + 15
                while not ready.exists():
                    assert server.poll() is None, "Export server exited before publishing factories"
                    assert time.monotonic() < deadline, "Export factories did not become ready"
                    time.sleep(0.025)
                for bitness in ("System32", "SysWOW64"):
                    powershell = Path(os.environ["WINDIR"]) / bitness / "WindowsPowerShell/v1.0/powershell.exe"
                    metadata = subprocess.run([str(powershell), "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(client),
                                    "-Directory", str(folder), "-Role", "metadata", "-MetadataOnly"],
                                   timeout=20, creationflags=NO_WINDOW, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
                    print(metadata.stdout, flush=True)
                    metadata.check_returncode()
                # The only host for this fresh config is started after metadata
                # checks. All output calls below reach the production Rust host.
                host = subprocess.Popen([str(host_exe), "--hub-host", "--hub-config", str(path)],
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, creationflags=NO_WINDOW)
                for bitness, role in (("System32", "first"), ("SysWOW64", "second")):
                    powershell = Path(os.environ["WINDIR"]) / bitness / "WindowsPowerShell/v1.0/powershell.exe"
                    children.append(subprocess.Popen([str(powershell), "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(client),
                                                      "-Directory", str(folder), "-Role", role], creationflags=NO_WINDOW,
                                                      stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True))
                for child in children:
                    output, _ = child.communicate(timeout=45)
                    print(output, flush=True)
                    assert child.returncode == 0, "Export COM client failed"
                assert server.poll() is None and host.poll() is None, "Clients stopped the shared server/host"
                print(f"{architecture} server: four stable outputs, both client bitnesses, independent leases and COM DeviceState passed", flush=True)
            finally:
                for process in children + [server, host]:
                    if process is not None:
                        if process.poll() is None:
                            process.kill()
                        process.wait(timeout=5)
                if host is not None:
                    output, errors = host.communicate(timeout=5)
                    if output or errors:
                        print("Simulation host diagnostics:", output, errors, flush=True)
                delete_tree = ctypes.windll.advapi32.RegDeleteTreeW
                delete_tree.argtypes = (ctypes.c_void_p, ctypes.c_wchar_p)
                delete_tree.restype = ctypes.c_long
                for view, key_path in reversed(created):
                    parent, name = key_path.rsplit("\\", 1)
                    with winreg.OpenKey(hive, parent, 0, winreg.KEY_ALL_ACCESS | view) as key:
                        status = delete_tree(int(key), name)
                        if status not in (0, 2):
                            raise OSError(status, "Cannot remove private export fixture registration")
                for signal in ("ready", "first-connected", "second-connected", "first-disconnected", "second-finished"):
                    (folder / signal).unlink(missing_ok=True)


if __name__ == "__main__":
    main()
