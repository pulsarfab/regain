"""Real COM activation in both bitnesses; never installed hardware drivers.

The private fixture CLSID is fail-if-present, removed in finally. Workers use
their production protocol/STA path without a test activation bypass. Local tests
use HKCU; elevated disposable GitHub runners explicitly use private HKLM keys.
"""
import argparse
import contextlib
import json
import os
from pathlib import Path
import queue
import subprocess
import struct
import tempfile
import threading
import time
import unittest
import uuid
import winreg

ROOT = Path(__file__).resolve().parents[1]
CLSID = "{E86CDFE1-0282-4A40-9265-F7D6A2CEBAF1}"
CLASS = "Regain.Hub.COM.Fixture.Driver"
ARCHITECTURES = ("x86", "x64")
WORKERS = ROOT / "target/debug"
FIXTURE = ROOT / "artifacts/hub-com-fixture/Regain.Hub.COM.Fixture.dll"
PROGID = "ASCOM.Rgn.F." + uuid.uuid4().hex[:16]
SELF_INSTANCE, SELF_OUTPUT = uuid.uuid4(), uuid.uuid4()
SELF_CLSID = "{" + str(uuid.uuid5(uuid.NAMESPACE_URL,
    f"https://pulsarfab.com/regain/ascom-hub/output/{SELF_INSTANCE}/{SELF_OUTPUT}/switch")) + "}"
FIXTURE_HIVE = winreg.HKEY_CURRENT_USER


def hresult(value):
    return value - 2**32 if value >= 2**31 else value


@contextlib.contextmanager
def registered_fixture():
    import ctypes
    delete_tree = ctypes.windll.advapi32.RegDeleteTreeW
    delete_tree.argtypes = (ctypes.c_void_p, ctypes.c_wchar_p)
    delete_tree.restype = ctypes.c_long
    # Registered codebase identity is checked from the compiled managed fixture.
    identity = subprocess.check_output([
        "powershell", "-NoProfile", "-Command",
        "[Reflection.AssemblyName]::GetAssemblyName($env:REGAIN_COM_FIXTURE_DLL).FullName",
    ], env={**os.environ, "REGAIN_COM_FIXTURE_DLL": str(FIXTURE)}, text=True).strip()
    assert identity.startswith("Regain.Hub.COM.Fixture,")
    progids = [PROGID] + [PROGID + "." + name for name in ("Switch", "Safety", "Weather", "Other", "Own", "Focuser", "Rotator", "Wheel", "Panel", "Camera")]
    classids = [CLSID, SELF_CLSID]
    paths = [f"Software\\Classes\\{name}" for name in progids] + [f"Software\\Classes\\CLSID\\{classid}" for classid in classids]
    views = (winreg.KEY_WOW64_32KEY, winreg.KEY_WOW64_64KEY)
    # Check both hives so a private test can never shadow a machine/user class.
    # Elevated COM ignores per-user class registrations even when HKCR's merged
    # view displays them. Machine registration is explicit and CI-only below.
    for hive in (winreg.HKEY_CURRENT_USER, winreg.HKEY_LOCAL_MACHINE):
        for view in views:
            for path in paths:
                try:
                    with winreg.OpenKey(hive, path, 0, winreg.KEY_READ | view):
                        raise RuntimeError(f"Refusing to replace existing fixture key: {path}")
                except FileNotFoundError:
                    pass
    # Delete only these exact, preflighted private fixture keys. RegDeleteTreeW
    # avoids moving registry keys between views or touching other registrations.
    created = []
    try:
        for view in views:
            for path in paths:
                created.append((view, path))
                with winreg.CreateKeyEx(FIXTURE_HIVE, path, 0, winreg.KEY_WRITE | view):
                    pass
            entries = {
                **{f"Software\\Classes\\{name}\\CLSID": {"": SELF_CLSID if name.endswith(".Own") else CLSID} for name in progids},
                **{f"Software\\Classes\\CLSID\\{classid}\\InprocServer32": {
                    "": "mscoree.dll", "ThreadingModel": "Both", "Class": CLASS,
                    "Assembly": identity, "RuntimeVersion": "v4.0.30319", "CodeBase": FIXTURE.as_uri(),
                } for classid in classids},
            }
            for path, values in entries.items():
                with winreg.CreateKeyEx(FIXTURE_HIVE, path, 0, winreg.KEY_WRITE | view) as key:
                    for name, value in values.items():
                        winreg.SetValueEx(key, name, 0, winreg.REG_SZ, value)
        yield
    finally:
        for view, path in reversed(created):
            parent, name = path.rsplit("\\", 1)
            with winreg.OpenKey(FIXTURE_HIVE, parent, 0, winreg.KEY_ALL_ACCESS | view) as key:
                status = delete_tree(int(key), name)
                if status not in (0, 2):
                    raise OSError(status, f"Failed to remove private fixture key {path}")


class Worker:
    def __init__(self, architecture, device="switch", policy="managed", settings=None, progid=PROGID, denied=None):
        self.temporary = tempfile.TemporaryDirectory(prefix="hub-com-", dir=ROOT / "artifacts")
        self.state = Path(self.temporary.name) / "state.json"
        self.settings = settings or {}
        self.set(**self.settings)
        self.started = time.monotonic()
        self.process = subprocess.Popen([
            str(WORKERS / "hub-ascom" / architecture / "Regain.Hub.ASCOM.exe"),
            "--import", "--prog-id", progid, "--device-type", device,
            "--connection-policy", policy, "--bitness", architecture,
        ] + (["--deny-clsids", ",".join(denied)] if denied else []), stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env={**os.environ, "REGAIN_HUB_COM_FIXTURE_STATE": str(self.state)})
        self.responses = queue.Queue()
        self.id = 0
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def set(self, **settings):
        self.settings.update(settings)
        temporary = self.state.with_suffix(".new")
        temporary.write_text(json.dumps(self.settings), encoding="utf-8")
        temporary.replace(self.state)

    def _read(self):
        try:
            for line in self.process.stdout:
                self.responses.put(line)
                # Only the image operation acknowledges a descriptor followed
                # by binary bytes. Read exactly its finite body and trailer so
                # embedded newline bytes cannot corrupt subsequent scalar frames.
                envelope = json.loads(line)
                result = envelope.get("result", {})
                descriptor = result.get("value")
                if isinstance(descriptor, dict) and descriptor.get("order") == "ascom":
                    sizes = {"int16": 2, "int32": 4, "double": 8, "single": 4, "uInt64": 8, "byte": 1, "int64": 8, "uInt16": 2, "uInt32": 4}
                    length = descriptor["width"] * descriptor["height"] * (descriptor["planes"] or 1) * sizes[descriptor["transmissionType"]]
                    assert length <= 512 * 1024 * 1024
                    self.responses.put(self.process.stdout.read(44 + length + 8))
        finally:
            self.responses.put(None)

    def send(self, operation, member=None, parameters=None, timeout=5):
        self.id += 1
        request = {"protocol": 1, "id": self.id, "operation": operation}
        if member is not None:
            request["member"] = member
        if parameters is not None:
            request["parameters"] = parameters
        self.raw(json.dumps(request).encode() + b"\n")
        response = self.responses.get(timeout=timeout)
        assert response is not None, "Worker exited without acknowledgement"
        assert len(response) <= 1024 * 1024
        assert b"PRIVATE_FIXTURE_SECRET" not in response
        envelope = json.loads(response)
        assert envelope["ok"] is True
        result = envelope["result"]
        assert result["protocol"] == 1 and result["id"] == self.id
        return result

    def raw(self, frame):
        self.process.stdin.write(frame)
        self.process.stdin.flush()

    def connect(self):
        for _ in range(30):
            response = self.send("connectStep")
            assert response["error"] is None, response
            if response["value"] is True:
                return response["connection"]
        raise AssertionError("Connect never completed")

    def disconnect(self):
        for _ in range(30):
            response = self.send("disconnectStep")
            assert response["error"] is None, response
            if response["value"] is True:
                return response["connection"]
        raise AssertionError("Disconnect never completed")

    def trace(self):
        path = Path(str(self.state) + ".trace")
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()] if path.exists() else []

    def count(self, member):
        return sum(item["member"] == member for item in self.trace())

    def expect_exit(self, timeout):
        try:
            return self.process.wait(timeout=timeout)
        except subprocess.TimeoutExpired as error:
            # Fixture-only diagnostics: preserve the failing deadline and report
            # whether any driver call or response preceded the missing exit.
            members = [item["member"] for item in self.trace()]
            raise AssertionError(
                f"Worker exit exceeded {timeout}s; elapsed={time.monotonic() - self.started:.3f}s; "
                f"pid={self.process.pid}; returncode={self.process.poll()}; "
                f"reader_alive={self.reader.is_alive()}; queued_responses={self.responses.qsize()}; "
                f"fixture_calls={members}"
            ) from error

    def close(self):
        if not self.process.stdin.closed:
            self.process.stdin.close()
        try:
            self.process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=3)
        self.reader.join(timeout=3)
        self.process.stdout.close()
        self.process.stderr.close()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()
        self.temporary.cleanup()


class ImportTests(unittest.TestCase):
    def test_camera_pulse_guide_enum_validation_async_blocking_and_hresult(self):
        for architecture in self.each():
            with Worker(architecture, device="camera", settings={"version": 4}) as worker:
                worker.connect()
                for args in ({"Direction": -1, "Duration": 1}, {"Direction": 4, "Duration": 1},
                             {"Direction": True, "Duration": 1}, {"Direction": 1.0, "Duration": 1},
                             {"Direction": 0, "Duration": -1}, {"Direction": 0, "Duration": "1"},
                             {"Direction": 0, "Duration": True}, {"Direction": 0, "Duration": 2**31},
                             {"Direction": 0}, {"Direction": 0, "Duration": 0, "Extra": 1}):
                    self.assertEqual(worker.send("write", "pulseguide", args)["error"]["kind"], "invalidValue")
                self.assertEqual(worker.count("PulseGuide"), 0)
                for direction in range(4):
                    self.assertIsNone(worker.send("write", "pulseguide", {"Direction": direction, "Duration": 0})["error"])
                    self.assertFalse(worker.send("read", "ispulseguiding")["value"])
                self.assertIsNone(worker.send("write", "pulseguide", {"Direction": 2, "Duration": 2**31-1})["error"])
                self.assertTrue(worker.send("read", "ispulseguiding")["value"])
                worker.set(cameraBlockingGuide=True)
                self.assertIsNone(worker.send("write", "pulseguide", {"Direction": 3, "Duration": 10})["error"])
                self.assertFalse(worker.send("read", "ispulseguiding")["value"])
                worker.set(faultMember="PulseGuide", faultCode=hresult(0x80040400))
                self.assertEqual(worker.send("write", "pulseguide", {"Direction": 0, "Duration": 1})["error"],
                                 {"kind": "unsupported", "code": hresult(0x80040400)})
                self.assertEqual(worker.count("Applied.PulseGuide"), 6)
                self.assertEqual(worker.count("AbortExposure"), 0)
                self.assertTrue(all(item["apartment"] == "STA" for item in worker.trace()))

    def test_camera_binary_all_types_ranks_and_scalar_framing(self):
        types = [("int16", "h", lambda n: -n % -30000), ("int32", "i", lambda n: -n),
                 ("double", "d", lambda n: n + .25), ("single", "f", lambda n: n + .5),
                 ("uInt64", "Q", lambda n: 2**63 + n), ("byte", "B", lambda n: n % 256),
                 ("int64", "q", lambda n: -(2**63) + n), ("uInt16", "H", lambda n: 40000 + n % 20000),
                 ("uInt32", "I", lambda n: 2**31 + n)]
        for architecture in self.each():
            with Worker(architecture, device="camera", settings={"version": 4, "imageWidth": 4, "imageHeight": 3}) as worker:
                self.assertEqual(worker.connect()["method"], "async")
                worker.send("write", "startexposure", {"Duration": .01, "Light": True})
                count = 0
                for name, encoding, pixel in types:
                    for planes in (0, 1, 3):
                        with self.subTest(architecture=architecture, type=name, planes=planes):
                            worker.set(imageType=name, imagePlanes=planes, imageLowerBounds=True)
                            reply = worker.send("image")
                            self.assertIsNone(reply["error"], reply)
                            self.assertEqual(reply["value"], {"width": 4, "height": 3, "planes": planes or None,
                                "elementType": name, "transmissionType": name, "order": "ascom"})
                            body = worker.responses.get(timeout=5)
                            fields = struct.unpack("<11I", body[:44])
                            self.assertEqual(fields, (1, 0, worker.id, 0, 44, types.index((name, encoding, pixel)) + 1,
                                                      types.index((name, encoding, pixel)) + 1, 3 if planes else 2, 4, 3, planes))
                            expected = b"".join(struct.pack("<" + encoding, pixel(n + 1)) for n in range(12 * (planes or 1)))
                            self.assertEqual(body[44:-8], expected)
                            self.assertEqual(body[-8:], b"RGNIMAGE")
                            count += 1
                            self.assertEqual(worker.send("read", "name")["value"], "COM fixture")
                self.assertEqual(worker.count("ImageArray"), count)
                self.assertEqual(worker.count("StartExposure"), 1)
                self.assertEqual(worker.count("StopExposure"), 0)
                self.assertEqual(worker.count("AbortExposure"), 0)
                self.assertTrue(all(item["apartment"] == "STA" for item in worker.trace()))

    def test_camera_types_setters_errors_and_connection_ownership(self):
        for architecture in self.each():
            for version in (2, 3, 4):
                with self.subTest(architecture=architecture, version=version), Worker(architecture, device="camera", settings={"version": version}) as worker:
                    info = worker.connect()
                    self.assertEqual(info["method"], "async" if version == 4 else "legacy")
                    self.assertEqual(worker.count("Connect"), int(version == 4))
                    for member, value in [("binx", 2), ("gain", 1), ("offset", 1), ("cooleron", True), ("setccdtemperature", -15), ("subexposureduration", .01)]:
                        name = {"binx": "BinX", "gain": "Gain", "offset": "Offset", "cooleron": "CoolerOn",
                                "setccdtemperature": "SetCCDTemperature", "subexposureduration": "SubExposureDuration"}[member]
                        reply = worker.send("write", member, {name: value})
                        self.assertIsNone(reply["error"], reply)
                        self.assertEqual(worker.send("read", member)["value"], value)
                    for member in ("gains", "offsets", "readoutmodes"):
                        self.assertEqual(worker.send("read", member)["value"], ["Low", "High"] if member != "readoutmodes" else ["Normal", "Fast"])
                    self.assertEqual(worker.send("image")["error"]["kind"], "unavailable")
                    self.assertEqual(worker.count("ImageArray"), 0)
                    for member, parameters in [("binx", {"BinX": 65536}), ("binx", {"BinX": 0}), ("gain", {"Gain": True}),
                            ("cooleron", {"CoolerOn": 1}), ("setccdtemperature", {"SetCCDTemperature": -274}),
                            ("startexposure", {"Duration": -1, "Light": True}), ("startexposure", {"Duration": .1, "Light": 1}),
                            ("stopexposure", {"Extra": 1})]:
                        self.assertEqual(worker.send("write", member, parameters)["error"]["kind"], "invalidValue")
                    self.assertEqual(worker.count("BinX.set"), 1)
                    self.assertEqual(worker.count("StartExposure"), 0)
                    worker.set(cameraCameraState=6, cameraImageReady="true", cameraGains=[1])
                    for member in ("camerastate", "imageready", "gains"):
                        self.assertEqual(worker.send("read", member)["error"]["kind"], "unavailable")
                    worker.set(argumentFaultMember="CoolerOn.set")
                    self.assertEqual(worker.send("write", "cooleron", {"CoolerOn": False})["error"]["kind"], "uncertain")
                    self.assertEqual(worker.send("write", "abortexposure")["error"]["kind"], "uncertain")
                    self.assertEqual(worker.count("AbortExposure"), 0)
                    worker.disconnect()
                    self.assertEqual(worker.count("Dispose"), 0)

    def test_camera_invalid_arrays_and_upstream_image_errors_stay_framed(self):
        for architecture in self.each():
            with Worker(architecture, device="camera", settings={"version": 4, "imageWidth": 2, "imageHeight": 3}) as worker:
                worker.connect()
                worker.send("write", "startexposure", {"Duration": .01, "Light": True})
                for settings in ({"imageRank": 1}, {"imageRank": 4}, {"imageRank": 2, "imageType": "invalid"}, {"imageRank": 2, "imageWidth": 0}):
                    worker.set(**settings)
                    self.assertEqual(worker.send("image")["error"]["kind"], "unavailable")
                    self.assertEqual(worker.send("read", "name")["value"], "COM fixture")
                worker.set(faultMember="ImageArray", faultCode=hresult(0x80040400))
                self.assertEqual(worker.send("image")["error"], {"kind": "unsupported", "code": hresult(0x80040400)})
                self.assertEqual(worker.send("read", "name")["value"], "COM fixture")

    def test_panel_legacy_modern_states_nonblocking_commands_and_connection_versions(self):
        for architecture in self.each():
            for version in (1, 2):
                with self.subTest(architecture=architecture, version=version), Worker(architecture, device="covercalibrator", settings={"version": version}) as worker:
                    info = worker.connect()
                    self.assertEqual(info["method"], "async" if version == 2 else "legacy")
                    self.assertEqual(worker.count("Connect"), int(version == 2))
                    self.assertEqual(worker.count("Connected.set"), int(version == 1))
                    for member, value in (("brightness", 0), ("maxbrightness", 4096), ("coverstate", 1), ("calibratorstate", 1)):
                        reply = worker.send("read", member)
                        self.assertIsNone(reply["error"], reply)
                        self.assertEqual(reply["value"], value)
                    for member in ("covermoving", "calibratorchanging"):
                        reply = worker.send("read", member)
                        if version == 1:
                            self.assertEqual(reply["error"]["kind"], "unsupported")
                        else:
                            self.assertIsNone(reply["error"], reply)
                            self.assertIs(reply["value"], False)
                    self.assertIsNone(worker.send("write", "opencover")["error"])
                    self.assertEqual(worker.send("read", "coverstate")["value"], 2)
                    if version == 2:
                        self.assertIs(worker.send("read", "covermoving")["value"], True)
                    self.assertIsNone(worker.send("write", "haltcover")["error"])
                    self.assertEqual(worker.send("read", "coverstate")["value"], 4)
                    self.assertIsNone(worker.send("write", "closecover")["error"])
                    self.assertIsNone(worker.send("write", "calibratoron", {"Brightness": 0})["error"])
                    self.assertEqual(worker.send("read", "calibratorstate")["value"], 2)
                    self.assertEqual(worker.send("read", "brightness")["value"], 0)
                    if version == 2:
                        self.assertIs(worker.send("read", "calibratorchanging")["value"], True)
                    self.assertIsNone(worker.send("write", "calibratoron", {"Brightness": 2147483647})["error"])
                    self.assertEqual(worker.send("read", "brightness")["value"], 2147483647)
                    self.assertIsNone(worker.send("write", "calibratoroff")["error"])
                    self.assertEqual(worker.send("read", "calibratorstate")["value"], 1)
                    worker.disconnect()
                    self.assertEqual(worker.count("Disconnect"), int(version == 2))
                    self.assertEqual(worker.count("SetupDialog"), 0)
                    self.assertEqual({t["apartment"] for t in worker.trace()}, {"STA"})
                    self.assertEqual(len({t["thread"] for t in worker.trace()}), 1)

    def test_panel_strict_values_and_arguments_before_driver_dispatch(self):
        for architecture in self.each():
            with self.subTest(architecture=architecture), Worker(architecture, device="covercalibrator", settings={"version": 2}) as worker:
                worker.connect()
                for member, bad, good in (("Brightness", [-1, 1.5, "1", True, 2147483648], 0),
                                          ("MaxBrightness", [0, -1, 1.5, "1", 2147483648], 2147483647),
                                          ("CoverState", [-1, 6, 1.5, "1", True], 5),
                                          ("CalibratorState", [-1, 6, 1.5, "1", True], 0),
                                          ("CoverMoving", [0, "false"], False),
                                          ("CalibratorChanging", [1, "true"], True)):
                    for value in bad:
                        worker.set(**{"panel" + member: value})
                        reply = worker.send("read", member.lower())
                        self.assertEqual(reply["error"]["kind"], "unavailable", (member, value, reply))
                    worker.set(**{"panel" + member: good})
                    self.assertIsNone(worker.send("read", member.lower())["error"])
                for args in ({}, {"Brightness": -1}, {"Brightness": 2147483648}, {"Brightness": 1.5},
                             {"Brightness": "1"}, {"Brightness": True}, {"brightness": 1}, {"Brightness": 1, "extra": True}):
                    self.assertEqual(worker.send("write", "calibratoron", args)["error"]["kind"], "invalidValue")
                for member in ("opencover", "closecover", "haltcover", "calibratoroff"):
                    self.assertEqual(worker.send("write", member, {"extra": True})["error"]["kind"], "invalidValue")
                for member in ("CalibratorOn", "OpenCover", "CloseCover", "HaltCover", "CalibratorOff"):
                    self.assertEqual(worker.count(member), 0)
                for member in ("move", "halt", "action", "commandblind"):
                    self.assertEqual(worker.send("write", member)["error"]["kind"], "unsupported")

    def test_panel_applied_on_with_unknown_reply_fences_all_mutations(self):
        for architecture in self.each():
            with self.subTest(architecture=architecture), Worker(architecture, device="covercalibrator", settings={"version": 2, "panelFaultAfterOn": True}) as worker:
                worker.connect()
                self.assertEqual(worker.send("write", "calibratoron", {"Brightness": 17})["error"]["kind"], "uncertain")
                self.assertEqual(worker.count("CalibratorOn.applied"), 1)
                self.assertEqual(worker.send("read", "brightness")["value"], 17)
                worker.set(panelFaultAfterOn=False)
                for member, args in (("calibratoron", {"Brightness": 18}), ("calibratoroff", {}),
                                     ("opencover", {}), ("closecover", {}), ("haltcover", {})):
                    self.assertEqual(worker.send("write", member, args)["error"]["kind"], "uncertain")
                worker.disconnect()
                worker.close()
                self.assertEqual(worker.count("CalibratorOn"), 1)
                for member in ("CalibratorOff", "OpenCover", "CloseCover", "HaltCover", "Dispose"):
                    self.assertEqual(worker.count(member), 0)

    def test_wheel_metadata_short_setter_and_class_specific_connection_versions(self):
        for architecture in self.each():
            for version in (2, 3):
                with self.subTest(architecture=architecture, version=version), Worker(architecture, device="filterwheel", settings={"version": version}) as worker:
                    info = worker.connect()
                    self.assertEqual(info["method"], "async" if version == 3 else "legacy")
                    self.assertEqual(worker.count("Connect"), int(version == 3))
                    self.assertEqual(worker.count("Connected.set"), int(version == 2))
                    for member, value in [("names", ["L", "Hα", ""]), ("focusoffsets", [-12, 0, 17]), ("position", 0)]:
                        reply = worker.send("read", member)
                        self.assertIsNone(reply["error"], reply)
                        self.assertEqual(reply["value"], value)
                    self.assertIsNone(worker.send("write", "position", {"Position": 2})["error"])
                    self.assertEqual(worker.send("read", "position")["value"], 2)
                    worker.set(wheelMoving=True, wheelOffsetsCase="boundaries")
                    self.assertIsNone(worker.send("write", "position", {"Position": 1})["error"])
                    self.assertEqual(worker.send("read", "position")["value"], -1)
                    self.assertEqual(worker.send("read", "focusoffsets")["value"], [-2147483648, 0, 2147483647])
                    worker.disconnect()
                    self.assertEqual(worker.count("Disconnect"), int(version == 3))
                    self.assertEqual(worker.count("SetupDialog"), 0)
                    self.assertEqual({t["apartment"] for t in worker.trace()}, {"STA"})
                    self.assertEqual(len({t["thread"] for t in worker.trace()}), 1)

    def test_wheel_bounded_safearrays_strict_types_and_setter_rejections(self):
        for architecture in self.each():
            with self.subTest(architecture=architecture), Worker(architecture, device="filterwheel") as worker:
                worker.connect()
                for case in ("empty", "nested", "wrong", "many", "large", "unicode"):
                    worker.set(wheelNamesCase=case)
                    reply = worker.send("read", "names")
                    self.assertEqual(reply["error"]["kind"], "unavailable", (case, reply))
                worker.set(wheelNamesCase="default")
                for case in ("empty", "noZero", "fraction", "string", "overflow"):
                    worker.set(wheelOffsetsCase=case)
                    reply = worker.send("read", "focusoffsets")
                    self.assertEqual(reply["error"]["kind"], "unavailable", (case, reply))
                worker.set(wheelOffsetsCase="default", badWheelPosition=True)
                self.assertEqual(worker.send("read", "position")["error"]["kind"], "unavailable")
                worker.set(badWheelPosition=False)
                for args in ({"Position": -1}, {"Position": 1024}, {"Position": 1.5}, {"Position": "1"}, {"position": 1}, {"Position": 1, "extra": True}):
                    self.assertEqual(worker.send("write", "position", args)["error"]["kind"], "invalidValue")
                self.assertEqual(worker.count("Position.set"), 0)
                self.assertEqual(worker.send("write", "position", {"Position": 3})["error"]["kind"], "invalidValue")
                self.assertIsNone(worker.send("write", "position", {"Position": 2})["error"])
                self.assertEqual(worker.send("read", "position")["value"], 2)
                for member in ("move", "halt", "calibrate", "action", "commandblind"):
                    self.assertEqual(worker.send("write", member)["error"]["kind"], "unsupported")
                self.assertEqual(worker.count("Halt"), 0)

    def test_wheel_unknown_setter_result_fences_subsequent_commands_without_replay(self):
        for architecture in self.each():
            with self.subTest(architecture=architecture), Worker(architecture, device="filterwheel") as worker:
                worker.connect()
                worker.set(argumentFaultMember="Position.set")
                reply = worker.send("write", "position", {"Position": 2})
                self.assertEqual(reply["error"]["kind"], "uncertain")
                self.assertNotIn("PRIVATE_FIXTURE_SECRET", json.dumps(reply))
                worker.set(argumentFaultMember="")
                self.assertEqual(worker.send("write", "position", {"Position": 1})["error"]["kind"], "uncertain")
                self.assertEqual(worker.count("Position.set"), 1)
                self.assertEqual(worker.count("Halt"), 0)

    def test_rotator_legacy_modern_connections_and_source_coordinates(self):
        for architecture in self.each():
            for version in (2, 3, 4):
                with self.subTest(architecture=architecture, version=version), Worker(architecture, device="rotator", settings={"version": version}) as worker:
                    info = worker.connect()
                    self.assertEqual(info["method"], "async" if version == 4 else "legacy")
                    self.assertEqual(worker.count("Connect"), int(version == 4))
                    self.assertEqual(worker.count("Connected.set"), int(version != 4))
                    for member, value in [("canreverse", True), ("ismoving", False), ("position", 20),
                                          ("mechanicalposition", 350), ("targetposition", 20), ("reverse", False), ("stepsize", 0.02)]:
                        response = worker.send("read", member)
                        self.assertIsNone(response["error"], response)
                        self.assertEqual(response["value"], value)
                    self.assertIsNone(worker.send("write", "move", {"Position": -721.5})["error"])
                    self.assertEqual(worker.send("read", "targetposition")["value"], 18.5)
                    self.assertEqual(worker.send("read", "position")["value"], 20)
                    self.assertTrue(worker.send("read", "ismoving")["value"])
                    self.assertIsNone(worker.send("write", "halt")["error"])
                    self.assertIsNone(worker.send("write", "sync", {"Position": 15})["error"])
                    self.assertEqual(worker.send("read", "position")["value"], 15)
                    self.assertEqual(worker.send("read", "mechanicalposition")["value"], 350)
                    self.assertIsNone(worker.send("write", "moveabsolute", {"Position": 42.5})["error"])
                    self.assertEqual(worker.send("read", "targetposition")["value"], 42.5)
                    self.assertIsNone(worker.send("write", "halt")["error"])
                    self.assertIsNone(worker.send("write", "movemechanical", {"Position": 355})["error"])
                    self.assertEqual(worker.send("read", "mechanicalposition")["value"], 355)
                    self.assertEqual(worker.send("read", "targetposition")["value"], 20)
                    self.assertIsNone(worker.send("write", "reverse", {"Reverse": True})["error"])
                    self.assertTrue(worker.send("read", "reverse")["value"])
                    self.assertEqual([t["value"] for t in worker.trace() if t["member"] == "Move"], [-721.5])
                    worker.disconnect()
                    self.assertEqual(worker.count("Disconnect"), int(version == 4))
                    self.assertEqual(worker.count("SetupDialog"), 0)
                    self.assertEqual({t["apartment"] for t in worker.trace()}, {"STA"})
                    self.assertEqual(len({t["thread"] for t in worker.trace()}), 1)

    def test_rotator_strict_readings_and_single_conversion_boundaries(self):
        for architecture in self.each():
            with self.subTest(architecture=architecture), Worker(architecture, device="rotator", settings={"version": 4}) as worker:
                worker.connect()
                for member, setting in [("position", "badAngle"), ("ismoving", "badMoving"),
                                        ("canreverse", "badReverse"), ("reverse", "badReverse"),
                                        ("stepsize", "badStepSize"), ("stepsize", "tinyStepSize")]:
                    worker.set(**{setting: True})
                    self.assertEqual(worker.send("read", member)["error"]["kind"], "unavailable")
                    worker.set(**{setting: False})
                for member, args in [("move", {"Position": 1e39}), ("move", {"Position": "1"}),
                                     ("move", {"position": 1}), ("move", {"Position": 1, "extra": True}),
                                     ("moveabsolute", {"Position": 359.9999999}), ("moveabsolute", {"Position": -1e-50}),
                                     ("movemechanical", {"Position": 360}), ("sync", {"Position": -1}),
                                     ("reverse", {"Reverse": 1}), ("halt", {"extra": True})]:
                    response = worker.send("write", member, args)
                    self.assertEqual(response["error"]["kind"], "invalidValue", response)
                for member in ("Move", "MoveAbsolute", "MoveMechanical", "Sync", "Reverse.set", "Halt"):
                    self.assertEqual(worker.count(member), 0)
                worker.set(faultMember="StepSize", faultCode=hresult(0x80040400))
                self.assertEqual(worker.send("read", "stepsize")["error"]["kind"], "unsupported")
                worker.set(faultMember="MoveMechanical")
                self.assertEqual(worker.send("write", "movemechanical", {"Position": 10})["error"]["kind"], "unsupported")
                worker.set(faultMember="")
                self.assertIsNone(worker.send("write", "sync", {"Position": 10})["error"])

    def test_rotator_uncertainty_fences_all_commands_without_replay_or_halt(self):
        for architecture in self.each():
            for failing, args, dispatched in [("move", {"Position": -20}, "Move"), ("reverse", {"Reverse": True}, "Reverse.set")]:
                with self.subTest(architecture=architecture, failing=failing), Worker(architecture, device="rotator", settings={"version": 4, "argumentFaultMember": dispatched}) as worker:
                    worker.connect()
                    self.assertEqual(worker.send("write", failing, args)["error"]["kind"], "uncertain")
                    worker.set(argumentFaultMember="")
                    for member, parameters in [("move", {"Position": 1}), ("moveabsolute", {"Position": 1}),
                                               ("movemechanical", {"Position": 1}), ("sync", {"Position": 1}),
                                               ("reverse", {"Reverse": False}), ("halt", {})]:
                        self.assertEqual(worker.send("write", member, parameters)["error"]["kind"], "uncertain")
                    self.assertEqual(worker.count(dispatched), 1)
                    self.assertEqual(worker.count("Halt"), 0)

    def test_typed_accessory_borrowed_connections_and_alias_denial(self):
        for architecture in self.each():
            for device, versions, suffix in (("rotator", (3, 4), "Rotator"), ("filterwheel", (2, 3), "Wheel"), ("covercalibrator", (1, 2), "Panel"), ("camera", (2, 3, 4), "Camera")):
                for version in versions:
                    with self.subTest(architecture=architecture, device=device, version=version), Worker(architecture, device=device, policy="externallyManaged", settings={"version": version, "initialConnected": True}) as worker:
                        self.assertFalse(worker.connect()["ownsConnection"])
                        worker.disconnect()
                        worker.close()
                        for member in ("Connect", "Disconnect", "Connected.set", "Dispose"):
                            self.assertEqual(worker.count(member), 0)
                with Worker(architecture, device=device, progid=PROGID + "." + suffix, denied=[CLSID.strip("{}")]) as worker:
                    self.assertEqual(worker.send("connectStep")["error"]["kind"], "invalidValue")
                    self.assertEqual(worker.count("Activate"), 0)

    def test_focuser_legacy_modern_connections_and_typed_members(self):
        for architecture in self.each():
            for version in (3, 4):
                with self.subTest(architecture=architecture, version=version), Worker(architecture, device="focuser", settings={"version": version}) as worker:
                    info = worker.connect()
                    self.assertEqual(info["method"], "async" if version == 4 else "legacy")
                    self.assertEqual(worker.count("Connect"), 1 if version == 4 else 0)
                    self.assertEqual(worker.count("Connected.set"), 0 if version == 4 else 1)
                    for member, value in [("absolute", True), ("maxstep", 100000), ("maxincrement", 1000),
                                          ("position", 50), ("ismoving", False), ("tempcompavailable", True),
                                          ("tempcomp", False), ("stepsize", 1.25), ("temperature", 12.5)]:
                        response = worker.send("read", member)
                        self.assertIsNone(response["error"], response)
                        self.assertEqual(response["value"], value)
                    self.assertIsNone(worker.send("write", "move", {"Position": 70000})["error"])
                    self.assertEqual(worker.send("read", "position")["value"], 70000)
                    self.assertTrue(worker.send("read", "ismoving")["value"])
                    self.assertIsNone(worker.send("write", "halt")["error"])
                    self.assertIsNone(worker.send("write", "tempcomp", {"TempComp": True})["error"])
                    self.assertTrue(worker.send("read", "tempcomp")["value"])
                    worker.set(relative=True)
                    self.assertFalse(worker.send("read", "absolute")["value"])
                    self.assertEqual(worker.send("read", "position")["error"]["kind"], "unsupported")
                    self.assertIsNone(worker.send("write", "move", {"Position": -30})["error"])
                    self.assertEqual(worker.count("Move"), 2)
                    worker.disconnect()
                    self.assertEqual(worker.count("Disconnect"), 1 if version == 4 else 0)

    def test_focuser_strict_readings_and_parameters_fail_before_dispatch(self):
        for architecture in self.each():
            with self.subTest(architecture=architecture), Worker(architecture, device="focuser", settings={"version": 4}) as worker:
                worker.connect()
                for member, setting in [("maxstep", "badMaxStep"), ("ismoving", "badMoving"), ("stepsize", "badStepSize")]:
                    worker.set(**{setting: True})
                    self.assertEqual(worker.send("read", member)["error"]["kind"], "unavailable")
                    worker.set(**{setting: False})
                for member, args in [("move", {"Position": 1.5}), ("move", {"Position": 2147483648}),
                                     ("move", {"Position": "1"}), ("move", {"position": 1}),
                                     ("move", {"Position": 1, "extra": True}), ("halt", {"extra": True}),
                                     ("tempcomp", {"TempComp": 1})]:
                    self.assertEqual(worker.send("write", member, args)["error"]["kind"], "invalidValue")
                self.assertEqual(worker.count("Move"), 0)
                self.assertEqual(worker.count("Halt"), 0)
                self.assertEqual(worker.count("TempComp.set"), 0)

    def test_focuser_uncertain_move_is_never_replayed_or_followed_by_automatic_halt(self):
        for architecture in self.each():
            with self.subTest(architecture=architecture), Worker(architecture, device="focuser", settings={"version": 4, "faultMember": "Move", "faultCode": hresult(0x800404FF)}) as worker:
                worker.connect()
                self.assertEqual(worker.send("write", "move", {"Position": 70})["error"]["kind"], "uncertain")
                worker.set(faultMember="")
                for member, args in [("move", {"Position": 80}), ("halt", {}), ("tempcomp", {"TempComp": True})]:
                    self.assertEqual(worker.send("write", member, args)["error"]["kind"], "uncertain")
                self.assertEqual(worker.count("Move"), 1)
                self.assertEqual(worker.count("Halt"), 0)
                self.assertEqual(worker.count("TempComp.set"), 0)

    def test_registered_progid_alias_is_rejected_before_activation(self):
        for architecture in self.each():
            with self.subTest(architecture=architecture), Worker(architecture, progid=PROGID + ".Switch", denied=[CLSID.strip("{}")]) as worker:
                response = worker.send("connectStep")
                self.assertEqual(response["error"]["kind"], "invalidValue")
                self.assertEqual(worker.count("Activate"), 0)
                self.assertIsNotNone(worker.send("connectStep")["error"])
                self.assertEqual(worker.count("Activate"), 0)
    def each(self):
        return ARCHITECTURES

    def test_actual_bitness_sta_pump_metadata_and_switch(self):
        for architecture in self.each():
            with self.subTest(architecture=architecture), Worker(architecture) as worker:
                info = worker.connect()
                self.assertEqual(info, {"deviceType": "switch", "interfaceVersion": 3, "method": "async", "ownsConnection": True, "uncertain": False, "ready": True})
                for member, expected in [("name", "COM fixture"), ("maxswitch", 2)]:
                    response = worker.send("read", member)
                    self.assertIsNone(response["error"])
                    self.assertEqual(response["value"], expected)
                for member, expected in [("canwrite", True), ("getswitchname", "Fixture level"), ("minswitchvalue", 0), ("maxswitchvalue", 100), ("switchstep", 0.5)]:
                    self.assertEqual(worker.send("read", member, {"Id": 0})["value"], expected)
                self.assertIsNone(worker.send("write", "setswitchvalue", {"Id": 0, "Value": 7.5})["error"])
                self.assertEqual(worker.send("read", "getswitchvalue", {"Id": 0})["value"], 7.5)
                self.assertIsNone(worker.send("write", "setswitch", {"Id": 0, "State": False})["error"])
                self.assertFalse(worker.send("read", "getswitch", {"Id": 0})["value"])
                worker.disconnect()
                worker.close()
                trace = worker.trace()
                self.assertEqual({t["apartment"] for t in trace}, {"STA"})
                self.assertEqual(len({t["thread"] for t in trace}), 1)
                self.assertEqual({t["bitness"] for t in trace}, {32 if architecture == "x86" else 64})
                self.assertEqual(worker.count("Pumped"), 1)
                self.assertEqual(worker.count("Disconnect"), 1)
                self.assertEqual(worker.count("Dispose"), 0)

    def test_legacy_ownership_and_borrowed_connections(self):
        for architecture in self.each():
            for policy, initial, owned in [("managed", False, True), ("managed", True, False), ("externallyManaged", True, False)]:
                with self.subTest(architecture=architecture, policy=policy, initial=initial), Worker(architecture, policy=policy, settings={"version": 2, "initialConnected": initial}) as worker:
                    self.assertEqual(worker.connect()["ownsConnection"], owned)
                    worker.disconnect()
                    worker.close()
                    self.assertEqual(worker.count("Connected.set"), 2 if owned else 0)
                    self.assertEqual(worker.count("Connect"), 0)
                    self.assertEqual(worker.count("Dispose"), 0)

    def test_modern_managed_claims_private_connection_even_when_shared(self):
        for architecture in self.each():
            with self.subTest(architecture=architecture), Worker(architecture, settings={"initialConnected": True, "sharedConnected": True}) as worker:
                self.assertTrue(worker.connect()["ownsConnection"])
                info = worker.disconnect()
                self.assertFalse(info["ownsConnection"])
                self.assertFalse(info["ready"])
                worker.close()
                self.assertEqual(worker.count("Connect"), 1)
                self.assertEqual(worker.count("Disconnect"), 1)
                self.assertEqual(worker.count("Connected.set"), 0)

    def test_external_closed_device_never_changes_connection(self):
        for architecture in self.each():
            with self.subTest(architecture=architecture), Worker(architecture, policy="externallyManaged") as worker:
                responses = [worker.send("connectStep") for _ in range(3)]
                self.assertEqual(responses[-1]["error"]["kind"], "disconnected")
                self.assertFalse(responses[-1]["connection"]["ownsConnection"])
                worker.close()
                self.assertEqual(worker.count("Connected.set") + worker.count("Connect") + worker.count("Disconnect"), 0)

    def test_missing_interface_version_negotiates_legacy_without_modern_fallback(self):
        for architecture in self.each():
            with self.subTest(architecture=architecture), Worker(architecture, settings={"faultMember": "InterfaceVersion", "faultCode": hresult(0x80040400)}) as worker:
                info = worker.connect()
                self.assertIsNone(info["interfaceVersion"])
                self.assertEqual(info["method"], "legacy")
                worker.disconnect()
                self.assertEqual(worker.count("Connected.set"), 2)
            with self.subTest(architecture=architecture), Worker(architecture, settings={"faultMember": "Connect", "faultCode": hresult(0x80040400)}) as worker:
                replies = [worker.send("connectStep") for _ in range(4)]
                self.assertEqual(replies[-1]["error"]["kind"], "unsupported")
                worker.send("connectStep")
                self.assertEqual(worker.count("Connect"), 1)
                self.assertEqual(worker.count("Connected.set"), 0)

    def test_strict_safety_observations(self):
        for architecture in self.each():
            with self.subTest(architecture=architecture), Worker(architecture, device="safetymonitor") as worker:
                worker.connect()
                self.assertIs(worker.send("read", "issafe")["value"], True)
                worker.set(safe=False)
                self.assertIs(worker.send("read", "issafe")["value"], False)
                worker.set(badSafe=True)
                self.assertEqual(worker.send("read", "issafe")["error"]["kind"], "unavailable")

    def test_uncertain_connection_commands_are_consumed_once(self):
        for architecture in self.each():
            with self.subTest(architecture=architecture, command="Connect"), Worker(architecture, settings={"faultMember": "Connect", "faultCode": hresult(0x800404FF)}) as worker:
                responses = [worker.send("connectStep") for _ in range(4)]
                self.assertEqual(responses[-1]["error"]["kind"], "uncertain")
                self.assertTrue(responses[-1]["connection"]["uncertain"])
                self.assertEqual(worker.send("connectStep")["error"]["kind"], "uncertain")
                self.assertEqual(worker.send("disconnectStep")["error"]["kind"], "uncertain")
                worker.close()
                self.assertEqual(worker.count("Connect"), 1)
                self.assertEqual(worker.count("Disconnect"), 0)
            with self.subTest(architecture=architecture, command="Disconnect"), Worker(architecture) as worker:
                worker.connect()
                worker.set(faultMember="Disconnect", faultCode=hresult(0x800404FF))
                reply = worker.send("disconnectStep")
                self.assertEqual(reply["error"]["kind"], "uncertain")
                self.assertFalse(reply["connection"]["ownsConnection"])
                self.assertTrue(reply["connection"]["uncertain"])
                worker.send("disconnectStep")
                worker.close()
                self.assertEqual(worker.count("Disconnect"), 1)

    def test_failed_connection_verification_can_release_acknowledged_ownership(self):
        for architecture in self.each():
            with self.subTest(architecture=architecture), Worker(architecture, settings={"verifyDisconnected": True}) as worker:
                for _ in range(30):
                    response = worker.send("connectStep")
                    if response["error"] is not None:
                        break
                self.assertEqual(response["error"]["kind"], "permanent")
                self.assertTrue(response["connection"]["ownsConnection"])
                self.assertFalse(response["connection"]["uncertain"])
                worker.close()
                self.assertEqual(worker.count("Disconnect"), 1)

    def test_weather_sensor_age_errors_refresh_and_averaging(self):
        for architecture in self.each():
            with self.subTest(architecture=architecture), Worker(architecture, device="observingconditions", settings={"version": 2}) as worker:
                self.assertEqual(worker.connect()["method"], "async")
                self.assertEqual(worker.send("read", "temperature")["value"], 12.5)
                self.assertEqual(worker.send("read", "timesincelastupdate", {"SensorName": "temperature"})["value"], 3.5)
                self.assertEqual(worker.send("read", "sensordescription", {"SensorName": "temperature"})["value"], "Fixture Temperature")
                self.assertIsNone(worker.send("write", "averageperiod", {"AveragePeriod": 0.5})["error"])
                self.assertEqual(worker.send("read", "averageperiod")["value"], 0.5)
                self.assertIsNone(worker.send("refresh")["error"])
                worker.set(faultMember="Humidity", faultCode=hresult(0x80040402))
                error = worker.send("read", "humidity")["error"]
                self.assertEqual(error, {"kind": "unavailable", "code": hresult(0x80040402)})
                self.assertEqual(worker.send("read", "temperature")["value"], 12.5)
                self.assertEqual(worker.count("SetupDialog"), 0)

    def test_invalid_parameters_never_dispatch_and_members_are_whitelisted(self):
        for architecture in self.each():
            with self.subTest(architecture=architecture), Worker(architecture) as worker:
                worker.connect()
                for parameters in [{"Id": -1, "Value": 1}, {"Id": 32768, "Value": 1}, {"Id": "0", "Value": 1}, {"Id": 0, "Value": "1"}, {"Id": 0, "Value": 1, "extra": True}]:
                    self.assertEqual(worker.send("write", "setswitchvalue", parameters)["error"]["kind"], "invalidValue")
                self.assertEqual(worker.send("write", "setswitch", {"Id": 0, "State": "true"})["error"]["kind"], "invalidValue")
                for member in ("connected", "disconnect", "action", "commandblind", "setupdialog", "__GetType"):
                    self.assertEqual(worker.send("write", member)["error"]["kind"], "unsupported")
                self.assertEqual(worker.count("SetSwitchValue") + worker.count("SetSwitch") + worker.count("SetupDialog"), 0)

    def test_errors_preserve_hresult_without_vendor_exception_text(self):
        for architecture in self.each():
            for code, expected in [(0x80040400, "unsupported"), (0x80040401, "invalidValue"), (0x80040407, "disconnected"), (0x80040402, "unavailable"), (0x800404FF, "transient")]:
                with self.subTest(architecture=architecture, code=code), Worker(architecture) as worker:
                    worker.connect()
                    worker.set(faultMember="GetSwitchValue", faultCode=hresult(code))
                    self.assertEqual(worker.send("read", "getswitchvalue", {"Id": 0})["error"], {"kind": expected, "code": hresult(code)})
            with self.subTest(architecture=architecture), Worker(architecture) as worker:
                worker.connect()
                worker.set(faultMember="SetSwitchValue", faultCode=hresult(0x800404FF))
                self.assertEqual(worker.send("write", "setswitchvalue", {"Id": 0, "Value": 2})["error"]["kind"], "uncertain")
                worker.set(faultMember="")
                self.assertEqual(worker.send("write", "setswitchvalue", {"Id": 0, "Value": 3})["error"]["kind"], "uncertain")
                self.assertIsNone(worker.send("read", "getswitchvalue", {"Id": 0})["error"])
                self.assertEqual(worker.count("SetSwitchValue"), 1)
            with self.subTest(architecture=architecture, exception="ArgumentException"), Worker(architecture) as worker:
                worker.connect()
                worker.set(argumentFaultMember="SetSwitchValue")
                self.assertEqual(worker.send("write", "setswitchvalue", {"Id": 0, "Value": 2})["error"], {"kind": "uncertain", "code": hresult(0x80070057)})

    def test_hung_worker_and_lost_mutation_reply_do_not_block_other_workers(self):
        for architecture in self.each():
            for member, operation, parameters in [("IsSafe", "read", None), ("SetSwitchValue", "write", {"Id": 0, "Value": 5})]:
                device = "safetymonitor" if member == "IsSafe" else "switch"
                wire_member = "issafe" if member == "IsSafe" else "setswitchvalue"
                with self.subTest(architecture=architecture, member=member), Worker(architecture, device=device) as stalled, Worker(architecture) as healthy:
                    stalled.connect()
                    healthy.connect()
                    stalled.set(hangMember=member)
                    with self.assertRaises(queue.Empty):
                        stalled.send(operation, wire_member, parameters, timeout=0.2)
                    self.assertEqual(healthy.send("read", "name")["value"], "COM fixture")
                    stalled.process.kill()
                    stalled.process.wait(timeout=3)
                    self.assertEqual(stalled.count(member), 1)
                    self.assertEqual(stalled.count("Disconnect"), 0)
                    self.assertEqual(healthy.send("read", "maxswitch")["value"], 2)

    def test_eof_releases_only_acknowledged_owned_connection(self):
        for architecture in self.each():
            for policy, owned in [("managed", True), ("externallyManaged", False)]:
                with self.subTest(architecture=architecture, policy=policy), Worker(architecture, policy=policy, settings={"initialConnected": True}) as worker:
                    worker.connect()
                    worker.close()
                    self.assertEqual(worker.count("Disconnect"), 1 if owned else 0)
                    self.assertEqual(worker.count("Dispose"), 0)

    def test_malformed_frames_and_replays_are_terminal_without_activation(self):
        malformed = [b"{}\n", b"{\"protocol\":1,\"protocol\":1,\"id\":1,\"operation\":\"connectStep\"}\n",
                     b"{\"protocol\":2,\"id\":1,\"operation\":\"connectStep\"}\n", b"x" * 4096 + b"\n",
                     b"{\"protocol\":1,\"id\":1,\"operation\":\"setupDialog\"}\n",
                     b"{\"protocol\":1,\"id\":1,\"operation\":\"connectStep\",\"member\":\"arbitrary\"}\n",
                     b'{"protocol":1,"id":1,"operation":"write","parameters":{"Id":0,"Id":1}}\n',
                     b'{"protocol":1,"id":1,"operation":"read","member":"\xff"}\n',
                     b'{"protocol":1,"id":1,"operation":"write","parameters":{"Value":Infinity}}\n']
        for architecture in self.each():
            for ready in (False, True):
                for frame in malformed:
                    with self.subTest(architecture=architecture, ready=ready, frame=frame[:60]), Worker(architecture) as worker:
                        if ready:
                            # A disconnected getter acknowledges the running STA
                            # without activating the private COM class. Keep the
                            # three-second terminal-framing assertion independent
                            # of CLR/process startup on a loaded CI runner.
                            reply = worker.send("read", "name")
                            self.assertEqual(reply["error"]["kind"], "disconnected")
                        worker.raw(frame)
                        # Cold framing also stays covered: its total allowance is
                        # the ordinary five-second startup/request budget plus
                        # the same three-second process-exit budget.
                        self.assertEqual(worker.expect_exit(3 if ready else 8), 0)
                        self.assertEqual(worker.count("Activate"), 0)
                        self.assertIsNone(worker.responses.get(timeout=3))
            with self.subTest(architecture=architecture), Worker(architecture) as worker:
                worker.send("read", "name")  # valid disconnected response, no activation
                worker.raw(b'{"protocol":1,"id":1,"operation":"connectStep"}\n')
                self.assertEqual(worker.expect_exit(3), 0)
                self.assertEqual(worker.count("Activate"), 0)

    def test_oversized_numeric_input_is_rejected_before_vendor_call(self):
        for architecture in self.each():
            with self.subTest(architecture=architecture), Worker(architecture) as worker:
                worker.connect()
                worker.id += 1
                frame = ('{"protocol":1,"id":' + str(worker.id) + ',"operation":"write","member":"setswitchvalue","parameters":{"Id":0,"Value":1e309}}\n').encode()
                worker.raw(frame)
                response = json.loads(worker.responses.get(timeout=5))["result"]
                self.assertEqual(response["error"]["kind"], "invalidValue")
                self.assertEqual(worker.count("SetSwitchValue"), 0)

    def test_missing_registration_is_permanent_and_wrong_bitness_exits_before_activation(self):
        for architecture in self.each():
            with self.subTest(architecture=architecture), Worker(architecture, progid="ASCOM.Regain.Unregistered." + uuid.uuid4().hex) as worker:
                self.assertEqual(worker.send("connectStep")["error"]["kind"], "permanent")
                self.assertEqual(worker.count("Activate"), 0)
            process = subprocess.run([
                str(WORKERS / "hub-ascom" / architecture / "Regain.Hub.ASCOM.exe"), "--import",
                "--prog-id", PROGID, "--device-type", "switch", "--connection-policy", "managed",
                "--bitness", "x64" if architecture == "x86" else "x86",
            ], capture_output=True, timeout=5)
            self.assertEqual(process.returncode, 2)
            self.assertEqual(process.stdout, b"")


def diagnose_fixture_activation():
    # Run only after failed inert tests, while the exact private registration is
    # still held. No production exception sanitization or activation path changes.
    with tempfile.TemporaryDirectory(prefix="hub-com-probe-", dir=ROOT / "artifacts") as directory:
        state = Path(directory) / "state.json"
        state.write_text("{}", encoding="utf-8")
        environment = {**os.environ, "REGAIN_HUB_COM_FIXTURE_STATE": str(state),
            "REGAIN_HUB_COM_FIXTURE_PROGID": PROGID, "REGAIN_COM_FIXTURE_DLL": str(FIXTURE)}
        for architecture, folder in (("x86", "SysWOW64"), ("x64", "System32")):
            print(f"Diagnosing private {architecture} fixture registration", flush=True)
            subprocess.run([str(Path(os.environ["SystemRoot"]) / folder / "WindowsPowerShell/v1.0/powershell.exe"),
                "-NoProfile", "-STA", "-ExecutionPolicy", "Bypass", "-File", str(ROOT / "scripts/diagnose-hub-com-fixture.ps1")],
                env=environment, timeout=20, check=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--workers", type=Path, default=WORKERS)
    parser.add_argument("--fixture", type=Path, default=FIXTURE)
    parser.add_argument("--rust-tests", action="store_true")
    parser.add_argument("--machine-fixture", action="store_true",
        help="Private machine registration on elevated disposable GitHub Windows runners only")
    arguments = parser.parse_args()
    if arguments.machine_fixture:
        import ctypes
        if os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_OS") != "Windows" or not ctypes.windll.shell32.IsUserAnAdmin():
            parser.error("--machine-fixture requires an elevated disposable GitHub Windows runner")
        FIXTURE_HIVE = winreg.HKEY_LOCAL_MACHINE
    print("Private COM fixture registration: " + ("HKLM (disposable elevated runner)" if arguments.machine_fixture else "HKCU"), flush=True)
    WORKERS = arguments.workers.resolve()
    FIXTURE = arguments.fixture.resolve()
    with registered_fixture():
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(ImportTests)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
        if not result.wasSuccessful():
            diagnose_fixture_activation()
        if result.wasSuccessful() and arguments.rust_tests:
            with tempfile.TemporaryDirectory(prefix="hub-com-rust-", dir=ROOT / "artifacts") as fixture_directory:
                environment = {**os.environ, "REGAIN_TEST_WORKERS": str(WORKERS),
                    "REGAIN_HUB_COM_SELF_INSTANCE": str(SELF_INSTANCE), "REGAIN_HUB_COM_SELF_OUTPUT": str(SELF_OUTPUT),
                    "REGAIN_HUB_COM_FIXTURE_DIRECTORY": fixture_directory, "REGAIN_HUB_COM_FIXTURE_PROGID": PROGID,
                    "REGAIN_HUB_COM_FIXTURE_HELPER": str(ROOT / "artifacts/hub-com-helper/Regain.Hub.Shared.Helper.Fixture.exe")}
                environment.pop("REGAIN_HUB_COM_FIXTURE_STATE", None)
                tests = subprocess.run(["cargo", "test", "-j2", "-p", "regain-hub", "--test", "com", "--locked", "--", "--test-threads=1"],
                    cwd=ROOT, env=environment)
                if tests.returncode:
                    raise SystemExit(tests.returncode)
    raise SystemExit(0 if result.wasSuccessful() else 1)
