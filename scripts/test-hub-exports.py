"""Private real COM exports, simulation only; no installed driver activation."""
import argparse
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
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Thread
from urllib.parse import parse_qs

ROOT = Path(__file__).resolve().parents[1]
NO_WINDOW = subprocess.CREATE_NO_WINDOW


class AccessoryFixture:
    """Always loopback, never an installed/vendor driver or physical worker."""
    def __init__(self, kind="focuser"):
        self.kind = kind

    def __enter__(self):
        values = dict(absolute=True, maxstep=1000, maxincrement=100, tempcompavailable=True,
                      position=50, ismoving=False, tempcomp=False, temperature=-5.0, connected=False,
                      interfaceversion=3)
        if self.kind == "rotator":
            values = dict(canreverse=True, reverse=False, mechanicalposition=350.0, position=20.0,
                          targetposition=20.0, stepsize=0.02, ismoving=False, connected=False, interfaceversion=3)
        elif self.kind == "filterwheel":
            values = dict(names=["L", "Hα", ""], focusoffsets=[-12, 0, 17], position=0,
                          connected=False, connecting=False, interfaceversion=3)
        kind = self.kind

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def respond(self, value=None, code=0):
                body = json.dumps(dict(Value=value, ErrorNumber=code, ErrorMessage="")).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self):
                member = self.path.split("?", 1)[0].rsplit("/", 1)[-1]
                self.respond(values.get(member), 0 if member in values else 1024)

            def do_PUT(self):
                member = self.path.rsplit("/", 1)[-1]
                args = parse_qs(self.rfile.read(int(self.headers["Content-Length"])).decode())
                if member == "connected":
                    values["connected"] = args["Connected"][0].lower() == "true"
                elif member in ("connect", "disconnect") and kind == "filterwheel":
                    values["connected"] = member == "connect"
                elif member == "position" and kind == "filterwheel":
                    values["position"] = int(args["Position"][0])
                elif member == "move" and kind == "focuser":
                    values["position"] = int(args["Position"][0])
                elif member in ("move", "moveabsolute", "movemechanical") and kind == "rotator":
                    angle = float(args["Position"][0])
                    logical, physical = values["position"], values["mechanicalposition"]
                    target = (logical + angle if member == "move" else angle + logical - physical if member == "movemechanical" else angle) % 360
                    values.update(targetposition=target, position=target, mechanicalposition=(physical + target - logical) % 360)
                elif member == "sync" and kind == "rotator":
                    values.update(position=float(args["Position"][0]), targetposition=float(args["Position"][0]))
                elif member == "reverse" and kind == "rotator":
                    values["reverse"] = args["Reverse"][0].lower() == "true"
                elif member == "halt":
                    values["ismoving"] = False
                elif member == "tempcomp":
                    values["tempcomp"] = args["TempComp"][0].lower() == "true"
                else:
                    self.respond(code=1024)
                    return
                self.respond()

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = Thread(target=self.server.serve_forever)
        self.thread.start()
        self.values = values
        self.url = f"http://127.0.0.1:{self.server.server_port}"
        return self

    def __exit__(self, *_args):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


class ScmProcess:
    """Hold an OS handle to the verified private fixture process, not a reusable PID."""
    def __init__(self, pid, created, executable):
        self.api = ctypes.WinDLL("kernel32", use_last_error=True)
        self.api.OpenProcess.argtypes = (ctypes.c_ulong, ctypes.c_bool, ctypes.c_ulong)
        self.api.OpenProcess.restype = ctypes.c_void_p
        self.api.GetExitCodeProcess.argtypes = (ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong))
        self.api.TerminateProcess.argtypes = (ctypes.c_void_p, ctypes.c_uint)
        self.api.WaitForSingleObject.argtypes = (ctypes.c_void_p, ctypes.c_ulong)
        self.api.CloseHandle.argtypes = (ctypes.c_void_p,)
        self.api.GetProcessTimes.argtypes = (ctypes.c_void_p,) + (ctypes.POINTER(ctypes.c_ulonglong),) * 4
        self.api.QueryFullProcessImageNameW.argtypes = (ctypes.c_void_p, ctypes.c_ulong, ctypes.c_wchar_p,
                                                       ctypes.POINTER(ctypes.c_ulong))
        self.handle = self.api.OpenProcess(0x1000 | 0x100000 | 0x1, False, pid)
        if not self.handle:
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            times = [ctypes.c_ulonglong() for _ in range(4)]
            image = ctypes.create_unicode_buffer(32768)
            length = ctypes.c_ulong(len(image))
            if not self.api.GetProcessTimes(self.handle, *(ctypes.byref(value) for value in times)):
                raise ctypes.WinError(ctypes.get_last_error())
            if not self.api.QueryFullProcessImageNameW(self.handle, 0, image, ctypes.byref(length)):
                raise ctypes.WinError(ctypes.get_last_error())
            # CIM CreationDate has microsecond precision; compare FILETIME at
            # the same precision. A reused PID must never authorize termination.
            if times[0].value // 10 != created // 10 or image.value.casefold() != str(executable).casefold():
                raise RuntimeError("Private SCM process identity changed before handle acquisition")
        except Exception:
            self.api.CloseHandle(self.handle)
            self.handle = None
            raise

    def poll(self):
        code = ctypes.c_ulong()
        if not self.api.GetExitCodeProcess(self.handle, ctypes.byref(code)):
            raise ctypes.WinError(ctypes.get_last_error())
        return None if code.value == 259 else code.value

    def kill(self):
        if not self.api.TerminateProcess(self.handle, 1):
            raise ctypes.WinError(ctypes.get_last_error())

    def wait(self, timeout):
        if self.api.WaitForSingleObject(self.handle, int(timeout * 1000)) != 0:
            raise RuntimeError("Private SCM server did not exit")
        code = self.poll()
        self.close()
        return code

    def close(self):
        self.api.CloseHandle(self.handle)
        self.handle = None


def find_scm_server(executable, folder):
    powershell = Path(os.environ["WINDIR"]) / "System32/WindowsPowerShell/v1.0/powershell.exe"
    # Paths are data in environment variables, never interpolated shell code.
    query = """ConvertTo-Json -Compress -InputObject @((Get-CimInstance Win32_Process -Filter "Name='Regain.Hub.ASCOM.exe'" |
        Where-Object { $_.ExecutablePath -eq $env:REGAIN_EXPORT_FIXTURE_EXE -and
            $_.CommandLine.Contains('--bindings') -and
            $_.CommandLine.Contains($env:REGAIN_EXPORT_FIXTURE_PATH) } |
        ForEach-Object { @{ pid=$_.ProcessId; created=$_.CreationDate.ToUniversalTime().ToFileTimeUtc() } }))"""
    result = subprocess.run([str(powershell), "-NoProfile", "-Command", query],
                            env={**os.environ, "REGAIN_EXPORT_FIXTURE_EXE": str(executable),
                                 "REGAIN_EXPORT_FIXTURE_PATH": str(folder / "bindings.json")},
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                            timeout=15, creationflags=NO_WINDOW, check=True)
    ids = json.loads(result.stdout or "[]")
    assert len(ids) <= 1, "COM launched more than one server for the same bound fixture"
    return ScmProcess(ids[0]["pid"], ids[0]["created"], executable) if ids else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scm", action="store_true", help="Let COM launch the bound server from LocalServer32")
    parser.add_argument("--registered", action="store_true", help="Exercise the production machine registration helper (disposable CI only)")
    options = parser.parse_args()
    if options.registered:
        options.scm = True
    workers = Path(os.environ.get("REGAIN_TEST_WORKERS", ROOT / "target/debug")).resolve()
    host_exe = workers / "regain-alpaca.exe"
    powershell = Path(os.environ["WINDIR"]) / "System32/WindowsPowerShell/v1.0/powershell.exe"
    owner = subprocess.check_output([str(powershell), "-NoProfile", "-Command",
                                   "[Security.Principal.WindowsIdentity]::GetCurrent().User.Value"],
                                   text=True, creationflags=NO_WINDOW, timeout=10).strip()
    with AccessoryFixture() as focuser, AccessoryFixture("rotator") as rotator, AccessoryFixture("filterwheel") as wheel, tempfile.TemporaryDirectory(prefix="hub-export-", dir=ROOT / "artifacts") as directory:
        folder = Path(directory)
        config = json.loads((ROOT / "crates/regain-hub/examples/simulated-observatory.json").read_text())
        config["instanceId"], config["revision"] = str(uuid.uuid4()), str(uuid.uuid4())
        additional = copy.deepcopy(config["outputs"][0])
        additional.update(id=str(uuid.uuid4()), number=1, label="Second simulation controls")
        for channel in additional["device"]["channels"]:
            channel["id"] = str(uuid.uuid4())
        config["outputs"].append(additional)
        source_id = str(uuid.uuid4())
        config["sources"].append(dict(id=source_id, label="Private loopback focuser simulation",
                                     backend=dict(kind="alpaca", baseUrl=focuser.url, deviceType="focuser",
                                                  deviceNumber=19, connectionPolicy="managed")))
        config["outputs"].append(dict(id=str(uuid.uuid4()), number=4, label="Private focuser simulation",
                                      device=dict(kind="proxy", source=source_id, deviceType="focuser")))
        rotator_id = str(uuid.uuid4())
        config["sources"].append(dict(id=rotator_id, label="Private loopback rotator",
                                      polling=dict(pollSeconds=0.1, requestTimeoutSeconds=1.0),
                                      backend=dict(kind="alpaca", baseUrl=rotator.url, deviceType="rotator", deviceNumber=19, connectionPolicy="managed")))
        config["outputs"].append(dict(id=str(uuid.uuid4()), number=4, label="Private rotator simulation",
                                      device=dict(kind="proxy", source=rotator_id, deviceType="rotator")))
        wheel_id = str(uuid.uuid4())
        config["sources"].append(dict(id=wheel_id, label="Private loopback wheel",
                                     backend=dict(kind="alpaca", baseUrl=wheel.url, deviceType="filterwheel", deviceNumber=19, connectionPolicy="managed")))
        config["outputs"].append(dict(id=str(uuid.uuid4()), number=4, label="Private wheel simulation",
                                      device=dict(kind="proxy", source=wheel_id, deviceType="filterwheel")))
        for source in config["sources"]:
            source["polling"] = dict(pollSeconds=0.1, requestTimeoutSeconds=0.3, attemptsPerCycle=1,
                                     initialBackoffSeconds=0.05, backoffCapSeconds=0.05)
        path = folder / "configuration.json"
        path.write_text(json.dumps(config), encoding="utf-8")
        bindings, identities = [], []
        for output in config["outputs"]:
            kind = output["device"].get("deviceType") or dict(switch="switch", safety="safetymonitor", weather="observingconditions")[output["device"]["kind"]]
            name = f'https://pulsarfab.com/regain/ascom-hub/output/{config["instanceId"]}/{output["id"]}/{kind}'
            clsid = uuid.uuid5(uuid.NAMESPACE_URL, name)  # Independent identity derivation.
            progid = "Rgn.H" + dict(switch="S", safetymonitor="M", observingconditions="W", focuser="F", rotator="R", filterwheel="L")[kind] + "." + clsid.hex
            assert len(progid) == 39
            identities.append(dict(clsid=str(clsid), progid=progid, version=4 if kind in ("focuser", "rotator") else 2 if kind == "observingconditions" else 3))
            bindings.append(dict(configPath=str(path), instanceId=config["instanceId"], outputId=output["id"],
                                 deviceType=kind, label=output["label"], simulated=True))
        (folder / "identities.json").write_text(json.dumps(identities))
        saved = folder / "bindings.json"
        saved.write_text(json.dumps(dict(schemaVersion=1, revision=str(uuid.uuid4()), bindings=bindings)))
        elevated = bool(ctypes.windll.shell32.IsUserAnAdmin())
        if options.registered and not (elevated and os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("RUNNER_OS") == "Windows"):
            raise RuntimeError("Production registration fixtures require an elevated disposable GitHub Windows runner")
        if elevated and not (os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("RUNNER_OS") == "Windows"):
            raise RuntimeError("Elevated machine fixture registration is permitted only on disposable GitHub runners")
        hive = winreg.HKEY_LOCAL_MACHINE if elevated else winreg.HKEY_CURRENT_USER
        views = (winreg.KEY_WOW64_32KEY, winreg.KEY_WOW64_64KEY)
        client = ROOT / "scripts/test-hub-export-client.ps1"
        for architecture in (("x64",) if options.registered else ("x86", "x64")):
            focuser.values.update(position=50, tempcomp=False, connected=False)
            rotator.values.update(position=20.0, mechanicalposition=350.0, targetposition=20.0, reverse=False, connected=False)
            wheel.values.update(position=0, connected=False)
            server_exe = workers / "hub-ascom" / architecture / "Regain.Hub.ASCOM.exe"
            paths = [item for identity in identities for item in
                     (f'Software\\Classes\\CLSID\\{{{identity["clsid"]}}}', f'Software\\Classes\\{identity["progid"]}')]
            app_id = "{" + str(uuid.uuid4()) + "}"
            if options.registered:
                for identity, binding in zip(identities, bindings):
                    paths.extend((f'Software\\Classes\\AppID\\{{{identity["clsid"]}}}',
                                  f'Software\\ASCOM\\{dict(switch="Switch", safetymonitor="SafetyMonitor", observingconditions="ObservingConditions", focuser="Focuser", rotator="Rotator", filterwheel="FilterWheel")[binding["deviceType"]]} Drivers\\{identity["progid"]}',
                                  f'Software\\PulsarFab\\Regain\\HubExports\\{{{identity["clsid"]}}}'))
            else:
                paths.append(f'Software\\Classes\\AppID\\{app_id}')
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
                for arguments in (("--bindings", "relative.json"), ("--bindings", str(folder / "missing.json")),
                                  ("--bindings", str(saved), "--bindings", str(saved)),
                                  ("--bindings", str(saved), "--host", "relative.exe"), ("--unknown",),
                                  ("--owner-sid", "not-a-sid"), ("--owner-sid", "S-1-5-18")):
                    invalid = subprocess.run([str(server_exe), "--export", *arguments],
                                             timeout=5, creationflags=NO_WINDOW)
                    assert invalid.returncode == 2, "Invalid bound launch must fail before publishing factories"
                if options.registered:
                    # Collision checks above completed before cleanup ownership
                    # or helper writes. All mutations stay inside this finally.
                    created.extend((view, key_path) for view in views for key_path in paths)
                    revision = json.loads(saved.read_text())["revision"]
                    for binding in bindings:
                        result = subprocess.run([str(workers / "Regain.ASCOM.Register.exe"), "/hubregister", str(saved), revision,
                                                 binding["instanceId"], binding["outputId"], owner],
                                                timeout=15, creationflags=NO_WINDOW)
                        assert result.returncode == 0, "Production bound registration failed"
                for view in (() if options.registered else views):
                    for key_path in paths:
                        created.append((view, key_path))
                        with winreg.CreateKeyEx(hive, key_path, 0, winreg.KEY_WRITE | view):
                            pass
                    for identity in identities:
                        entries = {
                            f'Software\\Classes\\CLSID\\{{{identity["clsid"]}}}': "Regain private hub fixture",
                            f'Software\\Classes\\CLSID\\{{{identity["clsid"]}}}\\LocalServer32':
                                f'"{server_exe}" /Embedding --ready "{ready}" --owner-sid {owner} --bindings "{saved}" --host "{host_exe}"',
                            f'Software\\Classes\\CLSID\\{{{identity["clsid"]}}}\\ProgID': identity["progid"],
                            f'Software\\Classes\\{identity["progid"]}\\CLSID': "{" + identity["clsid"] + "}",
                        }
                        for key_path, value in entries.items():
                            with winreg.CreateKeyEx(hive, key_path, 0, winreg.KEY_WRITE | view) as key:
                                winreg.SetValueEx(key, "", 0, winreg.REG_SZ, value)
                                if key_path.endswith("\\LocalServer32"):
                                    winreg.SetValueEx(key, "ServerExecutable", 0, winreg.REG_SZ, str(server_exe))
                        with winreg.OpenKey(hive, f'Software\\Classes\\CLSID\\{{{identity["clsid"]}}}', 0, winreg.KEY_WRITE | view) as key:
                            winreg.SetValueEx(key, "AppID", 0, winreg.REG_SZ, app_id)
                    with winreg.OpenKey(hive, f'Software\\Classes\\AppID\\{app_id}', 0, winreg.KEY_WRITE | view) as key:
                        winreg.SetValueEx(key, "", 0, winreg.REG_SZ, "Regain private bound hub server")
                        if elevated:
                            winreg.SetValueEx(key, "RunAs", 0, winreg.REG_SZ, "Interactive User")
                environment = {**os.environ, "REGAIN_HUB_BINDINGS": str(folder / "wrong-bindings.json"),
                               "REGAIN_HUB_HOST": str(folder / "wrong-host.exe")}
                if not options.scm:
                    server = subprocess.Popen([str(server_exe), "--export", "--ready", str(ready),
                                               "--owner-sid", owner, "--bindings", str(saved), "--host", str(host_exe)], env=environment,
                                              creationflags=NO_WINDOW)
                    deadline = time.monotonic() + 15
                    while not ready.exists():
                        assert server.poll() is None, "Export server exited before publishing factories"
                        assert time.monotonic() < deadline, "Export factories did not become ready"
                        time.sleep(0.025)
                for bitness in ("System32", "SysWOW64"):
                    powershell = Path(os.environ["WINDIR"]) / bitness / "WindowsPowerShell/v1.0/powershell.exe"
                    metadata = subprocess.run([str(powershell), "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(client),
                                    "-Directory", str(folder), "-Role", "metadata", "-MetadataOnly", "-TraceLaunch"],
                                   timeout=20, creationflags=NO_WINDOW, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
                    print(metadata.stdout, flush=True)
                    if options.scm and server is None:
                        server = find_scm_server(server_exe, folder)
                    if not options.scm:
                        observed = find_scm_server(server_exe, folder)
                        assert observed is not None and observed.poll() is None, "Bound server identity was not verified"
                        observed.close()
                    metadata.check_returncode()
                    assert server is not None and server.poll() is None, "Private bound COM server is not running"
                # The only host for this fresh config is started after metadata
                # checks. All output calls below reach the production Rust host.
                host = subprocess.Popen([str(host_exe), "--hub-host", "--hub-config", str(path)],
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, creationflags=NO_WINDOW)
                for bitness, role in (("System32", "first"), ("SysWOW64", "second")):
                    powershell = Path(os.environ["WINDIR"]) / bitness / "WindowsPowerShell/v1.0/powershell.exe"
                    children.append(subprocess.Popen([str(powershell), "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(client),
                                                      "-Directory", str(folder), "-Role", role], creationflags=NO_WINDOW,
                                                      stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True))
                results = []
                for child in children:
                    output, _ = child.communicate(timeout=45)
                    print(output, flush=True)
                    results.append(child.returncode)
                # Always collect both peers: one client's signal timeout can
                # otherwise hide the other client's actual COM failure.
                assert all(result == 0 for result in results), "Export COM client failed"
                assert server.poll() is None and host.poll() is None, "Clients stopped the shared server/host"
                if options.registered:
                    saved.unlink()  # Removal must use the inventory, not the chooser file.
                    for identity in identities:
                        result = subprocess.run([str(workers / "Regain.ASCOM.Register.exe"), "/hubunregister", identity["clsid"], owner],
                                                timeout=15, creationflags=NO_WINDOW)
                        assert result.returncode == 0, "Production inventory-based removal failed"
                    for view in views:
                        for key_path in paths:
                            try:
                                with winreg.OpenKey(hive, key_path, 0, winreg.KEY_READ | view):
                                    raise AssertionError("Production removal left a registration key")
                            except FileNotFoundError:
                                pass
                    assert server.poll() is None and host.poll() is None, "Registration removal stopped a shared server"
                print(f"{architecture} {'SCM' if options.scm else 'manual'} server: seven stable outputs, both client bitnesses, independent leases and typed focuser/rotator/wheel DeviceState passed", flush=True)
            finally:
                startup = folder / "ready.startup"
                if startup.exists():
                    print("Private export startup:", startup.read_text(), flush=True)
                    startup.unlink()
                cleanup_errors = []
                if options.scm and server is None:
                    try:
                        server = find_scm_server(server_exe, folder)
                    except Exception as error:
                        cleanup_errors.append(error)
                for process in children + [server, host]:
                    if process is not None:
                        try:
                            if process.poll() is None:
                                process.kill()
                            process.wait(timeout=5)
                        except Exception as error:
                            cleanup_errors.append(error)
                if host is not None:
                    try:
                        output, errors = host.communicate(timeout=5)
                        if output or errors:
                            print("Simulation host diagnostics:", output, errors, flush=True)
                    except Exception as error:
                        cleanup_errors.append(error)
                delete_tree = ctypes.windll.advapi32.RegDeleteTreeW
                delete_tree.argtypes = (ctypes.c_void_p, ctypes.c_wchar_p)
                delete_tree.restype = ctypes.c_long
                for view, key_path in reversed(created):
                    try:
                        parent, name = key_path.rsplit("\\", 1)
                        with winreg.OpenKey(hive, parent, 0, winreg.KEY_ALL_ACCESS | view) as key:
                            status = delete_tree(int(key), name)
                            if status not in (0, 2):
                                raise OSError(status, "Cannot remove private export fixture registration")
                    except FileNotFoundError:
                        pass  # A failed registration may not have created its parent.
                    except Exception as error:
                        cleanup_errors.append(error)
                for signal in ("ready", "first-connected", "second-connected", "first-disconnected", "second-finished"):
                    (folder / signal).unlink(missing_ok=True)
                if cleanup_errors:
                    raise RuntimeError("Private COM export fixture cleanup failed") from cleanup_errors[0]


if __name__ == "__main__":
    main()
