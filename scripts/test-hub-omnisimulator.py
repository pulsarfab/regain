"""Exercise an already running ASCOM OmniSimulator through a private Regain hub.

Only accepts a loopback port, verifies the ASCOM simulator identity and requires
all eight inputs initially disconnected. Never accepts hardware/config/COM IDs.
One native FocusCube3 input uses explicit production-worker simulation. Retains
config, traffic, images, hashes and cleanup results in a fresh artifacts folder.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import socket
import struct
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
CLASSES = ("switch", "safetymonitor", "observingconditions", "focuser", "rotator",
           "filterwheel", "covercalibrator", "camera")
PENDING = {0x402, 0x407}
CAMERA_KEYS = {"startx": "StartX", "starty": "StartY", "numx": "NumX", "numy": "NumY"}
ASYNC_VERSION = {"camera": 4, "focuser": 4, "rotator": 4, "filterwheel": 3,
                 "switch": 3, "safetymonitor": 3, "covercalibrator": 2, "observingconditions": 2}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--simulator-port", type=int, default=32323)
    parser.add_argument("--bin-dir", type=Path, default=ROOT / "target/debug")
    args = parser.parse_args()
    if not __debug__:
        parser.error("Run without Python optimization; acceptance assertions must remain enabled")
    if not 1 <= args.simulator_port <= 65535:
        parser.error("Invalid loopback simulator port")
    suffix = ".exe" if os.name == "nt" else ""
    binary = (args.bin_dir / ("regain-alpaca" + suffix)).resolve(strict=True)
    worker = (binary.parent / ("regain-device" + suffix)).resolve(strict=True)
    directory = ROOT / "artifacts" / ("hub-omnisimulator-" + uuid.uuid4().hex)
    directory.mkdir()
    print(f"External OmniSimulator acceptance evidence: {directory}", flush=True)
    upstream = f"http://127.0.0.1:{args.simulator_port}"
    traffic, cleanup, results = [], [], []
    host = publisher = None
    leases = []
    camera_settings = None
    rejected_classes = []
    base = None
    transaction = 0
    interfaces = {}

    def call(origin, path, data=None, binary_response=False):
        nonlocal transaction
        transaction += 1
        headers = {"Accept": "application/imagebytes"} if binary_response else {}
        if data is not None:
            headers["Content-Type"] = "application/x-www-form-urlencoded"
            data = urllib.parse.urlencode(data).encode()
        request = urllib.request.Request(origin + path, data, headers, method="PUT" if data is not None else "GET")
        with urllib.request.urlopen(request, timeout=5) as response:
            body = response.read(4 * 1024 * 1024 + 1)
        if len(body) > 4 * 1024 * 1024:
            raise RuntimeError("Unexpected oversized simulator response")
        value = body if binary_response else json.loads(body)
        traffic.append({"origin": origin, "path": path, "method": request.get_method(),
                        "bytes": len(body), "response": None if binary_response else value})
        return value

    def value(origin, kind, member, client=73001, number=40, data=None, **query):
        parameters = {"ClientID": client, "ClientTransactionID": transaction + 1, **query}
        path = f"/api/v1/{kind}/{number}/{member}"
        if data is None:
            reply = call(origin, path + "?" + urllib.parse.urlencode(parameters))
        else:
            reply = call(origin, path, {**parameters, **data})
        if reply["ErrorNumber"]:
            raise AlpacaError(reply)
        return reply.get("Value")

    def wait(read, accept=lambda result: True, seconds=15):
        deadline = time.monotonic() + seconds
        while True:
            try:
                result = read()
                if accept(result):
                    return result
            except AlpacaError as error:
                if error.reply["ErrorNumber"] not in PENDING:
                    raise
            if time.monotonic() >= deadline:
                raise TimeoutError("Simulator/hub acceptance condition did not complete")
            time.sleep(0.05)

    def hub(kind, member, **kwargs):
        return value(base, kind, member, **kwargs)

    def original(kind, member, **kwargs):
        return value(upstream, kind, member, number=0, **kwargs)

    def original_connection(kind):
        return connection_state(original, kind, interfaces[kind])

    def save(name, value):
        (directory / name).write_text(json.dumps(value, indent=2), encoding="utf-8")

    description = call(upstream, "/management/v1/description")
    assert description["ErrorNumber"] == 0
    identity = description["Value"]
    assert identity["Manufacturer"] == "ASCOM Initiative", identity
    assert identity["ServerName"] == "ASCOM Alpaca Simulators", identity
    catalog = call(upstream, "/management/v1/configureddevices")
    assert catalog["ErrorNumber"] == 0
    devices = {item["DeviceType"].lower(): item for item in catalog["Value"] if item["DeviceNumber"] == 0}
    for kind in CLASSES:
        assert kind in devices and "sim" in devices[kind]["DeviceName"].lower(), devices.get(kind)
        interfaces[kind] = original(kind, "interfaceversion")
        assert original_connection(kind) == (False, False), f"{kind} is in use or connecting"
    save("upstream.json", {"description": description, "catalog": catalog})

    config = json.loads((ROOT / "crates/regain-hub/examples/simulated-observatory.json").read_text(encoding="utf-8"))
    config.update(instanceId=str(uuid.uuid4()), revision=str(uuid.uuid4()))
    for source in config["sources"]:
        kind = source["backend"]["deviceType"]
        source["id"] = str(uuid.uuid4())
        source["backend"] = {"kind": "alpaca", "baseUrl": upstream + "/", "deviceType": kind,
                             "deviceNumber": 0, "uniqueId": devices[kind]["UniqueID"], "connectionPolicy": "managed"}
        source["polling"] = {"pollSeconds": 0.1, "requestTimeoutSeconds": 1.0, "connectionTimeoutSeconds": 10.0}
        output = next(item for item in config["outputs"] if item["device"]["kind"] == {
            "switch": "switch", "safetymonitor": "safety", "observingconditions": "weather"}[kind])
        if kind == "switch":
            output["device"]["channels"] = [{"id": str(uuid.uuid4()), "number": 0, "label": "External simulator channel 0",
                "writable": False, "readout": {"kind": "channel", "source": source["id"], "channel": 0},
                "minimum": 0, "maximum": 1, "step": 1, "units": ""}]
        elif kind == "safetymonitor":
            output["device"]["members"] = [{"source": source["id"], "enabled": True,
                "policy": {"confirmationSeconds": 0.1, "maximumSafeAgeSeconds": 5.0, "returnToSafeHoldSeconds": 0.2}}]
        else:
            for measurement in output["device"]["measurements"].values():
                measurement["sources"][0]["source"] = source["id"]
    native = str(uuid.uuid4())
    config["sources"].append({"id": native, "label": "Explicit native FC3 simulation",
        "backend": {"kind": "native", "device": "fc3", "identity": "00:00:00:00:00:03"}, "polling": {"pollSeconds": 0.1}})
    config["outputs"][0]["device"]["channels"].append({"id": str(uuid.uuid4()), "number": 1,
        "label": "Native simulated temperature", "writable": False,
        "readout": {"kind": "property", "source": native, "property": "temperature"},
        "minimum": -100, "maximum": 100, "step": 0.1, "units": "°C"})
    for kind in CLASSES[3:]:
        source = str(uuid.uuid4())
        config["sources"].append({"id": source, "label": f"External simulator {kind}", "backend": {
            "kind": "alpaca", "baseUrl": upstream + "/", "deviceType": kind, "deviceNumber": 0,
            "uniqueId": devices[kind]["UniqueID"], "connectionPolicy": "managed"},
            "polling": {"pollSeconds": 0.1, "requestTimeoutSeconds": 1.0, "connectionTimeoutSeconds": 10.0}})
        config["outputs"].append({"id": str(uuid.uuid4()), "number": 40, "label": f"PRIVATE Simulation {kind}",
            "device": {"kind": "proxy", "source": source, "deviceType": kind}})
    for output in config["outputs"]:
        output.update(id=str(uuid.uuid4()), number=40, label="PRIVATE Simulation " + output["label"])
    save("hub.json", config)
    save("profiles.json", [])
    save("provenance.json", {"server": str(binary), "serverSha256": digest(binary), "worker": str(worker),
                            "workerSha256": digest(worker), "upstream": identity, "simulationOnly": True})
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
    failure = None
    with (directory / "host.log").open("w", encoding="utf-8") as host_log, \
            (directory / "publisher.log").open("w", encoding="utf-8") as publisher_log:
        try:
            common = ["--hub-config", str(directory / "hub.json"), "--workers", str(binary.parent), "--simulate"]
            host = subprocess.Popen([str(binary), "--hub-host", *common], stdout=host_log, stderr=host_log, creationflags=flags)
            wait(lambda: (directory / "host.log").read_text(encoding="utf-8"),
                 lambda text: "Regain hub ready:" in text if host.poll() is None else fail("Owned hub exited"))
            publisher = subprocess.Popen([str(binary), *common, "--profiles", str(directory / "profiles.json"),
                "--listen", "127.0.0.1", "--port", str(port), "--no-discovery"], stdout=publisher_log, stderr=publisher_log, creationflags=flags)
            deadline = time.monotonic() + 15
            while True:
                try:
                    published = call(base, "/management/v1/configureddevices")
                    assert published["ErrorNumber"] == 0 and len(published["Value"]) == 8
                    break
                except (urllib.error.URLError, TimeoutError):
                    if publisher.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError("Owned publisher failed to start")
                    time.sleep(0.05)
            for kind in CLASSES:
                leases.append((kind, 73001))
                try:
                    hub(kind, "connected", data={"Connected": "true"})
                except AlpacaError as error:
                    # Preserve an observed external standards failure while
                    # gathering evidence for the other classes. This remains a
                    # failing overall run; do not normalize upstream metadata.
                    if kind != "filterwheel" or error.reply["ErrorNumber"] != 0x402:
                        raise
                    offsets = original(kind, "focusoffsets")
                    if not isinstance(offsets, list) or not offsets or 0 in offsets:
                        raise
                    rejected_classes.append(kind)
                    results.append({"class": kind, "rejected": error.reply,
                        "upstreamFocusOffsets": offsets, "missingZeroReference": True})
                    print("Filter wheel rejected: upstream offsets have no zero reference", flush=True)
                    continue
                wait(lambda: hub(kind, "connected"), lambda connected: connected is True)
                print(f"Connected external {kind}", flush=True)
            for kind, members in {
                "focuser": ["absolute", "maxstep", "maxincrement", "ismoving"],
                "rotator": ["position", "mechanicalposition", "ismoving"],
                "filterwheel": ["names", "focusoffsets", "position"],
                "covercalibrator": ["coverstate", "calibratorstate", "brightness", "maxbrightness"],
            }.items():
                if kind in rejected_classes:
                    continue
                values = {member: wait(lambda member=member: hub(kind, member)) for member in members}
                results.append({"class": kind, "properties": values})
            for channel in (0, 1):
                reading = wait(lambda: hub("switch", "getswitchvalue", Id=channel))
                assert isinstance(reading, (float, int)) and math.isfinite(reading)
                results.append({"class": "switch", "channel": channel, "value": reading})
            for metric in ("temperature", "pressure"):
                reading = wait(lambda: hub("observingconditions", metric))
                assert isinstance(reading, (float, int)) and math.isfinite(reading)
                results.append({"class": "observingconditions", "metric": metric, "value": reading})
            raw_safe = original("safetymonitor", "issafe")
            assert type(raw_safe) is bool
            safe = wait(lambda: hub("safetymonitor", "issafe"), lambda current: current is raw_safe)
            results.append({"class": "safetymonitor", "value": safe})
            leases.append(("camera", 73002))
            hub("camera", "connected", client=73002, data={"Connected": "true"})
            wait(lambda: hub("camera", "connected", client=73002), lambda connected: connected is True)
            assert wait(lambda: hub("camera", "camerastate")) == 0, "Simulator camera is busy"
            camera_settings = {member: wait(lambda member=member: hub("camera", member)) for member in ("startx", "starty", "numx", "numy")}
            for member, number in {"startx": 0, "starty": 0, "numx": 32, "numy": 24}.items():
                hub("camera", member, data={CAMERA_KEYS[member]: number})
            hub("camera", "startexposure", data={"Duration": 0.05, "Light": "true"})
            wait(lambda: hub("camera", "imageready"), lambda ready: ready is True)
            image = hub("camera", "imagearray")
            assert len(image) == 32 and len(image[0]) == 24
            save("image.json", image)
            raw = call(base, "/api/v1/camera/40/imagearray?ClientID=73002", binary_response=True)
            (directory / "image.bin").write_bytes(raw)
            fields = struct.unpack_from("<11i", raw)
            version, error, _, _, offset, _, encoded, rank, x, y, z = fields
            assert version == 1 and error == 0 and rank in (2, 3) and (x, y) == (32, 24)
            formats = {1: "h", 2: "i", 3: "d", 4: "f", 5: "Q", 6: "B", 7: "q", 8: "H", 9: "I"}
            flat = flatten(image)
            assert len(flat) == x * y * (z if rank == 3 else 1)
            assert offset + len(flat) * struct.calcsize(formats[encoded]) == len(raw)
            pixels = struct.unpack_from("<" + formats[encoded] * len(flat), raw, offset)
            assert list(pixels) == flat, "Binary and JSON images differ"
            hub("camera", "connected", data={"Connected": "false"})
            leases.remove(("camera", 73001))
            assert hub("camera", "connected", client=73002) is True
            assert hub("camera", "imagearray", client=73002) == image
            results.append({"class": "camera", "pixels": len(flat), "binarySha256": hashlib.sha256(raw).hexdigest(),
                            "exactAcrossFormatsAndClients": True, "survivingSibling": True})
            print("Available classes, mixed native gauge and exact shared camera images passed", flush=True)
            if rejected_classes:
                raise RuntimeError(f"External acceptance incomplete: rejected {rejected_classes}")
        except Exception as error:
            failure = repr(error)
            diagnostics = []
            for source in config["sources"]:
                try:
                    request = urllib.request.Request(base + "/setup/api/hub",
                        json.dumps({"op": "sourceStatus", "source": source["id"]}).encode(),
                        {"Content-Type": "application/json", "Origin": base}, method="POST")
                    with urllib.request.urlopen(request, timeout=5) as response:
                        diagnostics.append({"source": source["label"], "reply": json.load(response)})
                except Exception as diagnostic_error:
                    diagnostics.append({"source": source["label"], "error": repr(diagnostic_error)})
            save("failure-diagnostics.json", diagnostics)
            raise
        finally:
            if camera_settings is not None:
                try:
                    client = 73002 if ("camera", 73002) in leases else 73001
                    for member in ("numx", "numy", "startx", "starty"):
                        hub("camera", member, client=client, data={CAMERA_KEYS[member]: camera_settings[member]})
                    restored = {member: hub("camera", member, client=client) for member in CAMERA_KEYS}
                    assert restored == camera_settings, restored
                    cleanup.append({"cameraSettingsRestored": True, "readback": restored})
                except Exception as error:
                    cleanup.append({"cameraSettingsRestored": False, "error": repr(error)})
            for kind, client in reversed(leases):
                try:
                    hub(kind, "connected", client=client, data={"Connected": "false"})
                    cleanup.append({"class": kind, "client": client, "released": True})
                except Exception as error:
                    cleanup.append({"class": kind, "client": client, "released": False, "error": repr(error)})
            for kind in CLASSES:
                try:
                    # Connected can still be false while an asynchronous open
                    # is pending. Require both flags quiet across three polls.
                    quiet = QuietDisconnect()
                    wait(lambda kind=kind: original_connection(kind), quiet)
                    cleanup.append({"class": kind, "upstreamDisconnected": True})
                except Exception as error:
                    cleanup.append({"class": kind, "upstreamDisconnected": False, "error": repr(error)})
            for process in (publisher, host):
                if process is not None:
                    if process.poll() is None:
                        process.terminate()
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)
            save("traffic.json", traffic)
            save("summary.json", {"simulationOnly": True, "results": results, "failure": failure,
                "cleanup": cleanup, "ownedProcessesStopped": all(p is None or p.poll() is not None for p in (host, publisher))})
    assert all(item.get("released", True) and item.get("cameraSettingsRestored", True)
               and item.get("upstreamDisconnected", True) for item in cleanup), cleanup
    print("Owned processes stopped; all simulator connections returned to disconnected", flush=True)


class AlpacaError(RuntimeError):
    def __init__(self, reply):
        self.reply = reply
        super().__init__(str(reply))


class QuietDisconnect:
    """Do not mistake a pending open or a one-poll gap for restored state."""
    def __init__(self):
        self.quiet = 0

    def __call__(self, state):
        self.quiet = self.quiet + 1 if all(flag is False for flag in state) and len(state) == 2 else 0
        return self.quiet >= 3


def connection_state(read, kind, version):
    connecting = read(kind, "connecting") if version >= ASYNC_VERSION[kind] else False
    connected = read(kind, "connected")
    if type(connecting) is not bool or type(connected) is not bool:
        raise RuntimeError("Invalid simulator connection flags")
    return connecting, connected


def fail(message):
    raise RuntimeError(message)


def digest(path):
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def flatten(value):
    if isinstance(value, list):
        return [number for item in value for number in flatten(item)]
    return [value]


if __name__ == "__main__":
    main()
