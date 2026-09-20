"""Run the standalone Rust HTTP server against simulated workers on any OS."""
import argparse
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", default="target/debug")
    args = parser.parse_args()
    executable = Path(args.bin_dir).resolve() / ("regain-alpaca.exe" if os.name == "nt" else "regain-alpaca")
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    base = f"http://127.0.0.1:{port}"

    def request(path, data=None, method=None, binary=False):
        headers = {}
        if data is not None:
            is_setup = path.startswith("/setup/")
            headers["Content-Type"] = "application/json" if is_setup else "application/x-www-form-urlencoded"
            data = (json.dumps(data) if is_setup else urllib.parse.urlencode(data)).encode()
        if binary:
            headers["Accept"] = "application/imagebytes"
        with urllib.request.urlopen(urllib.request.Request(base + path, data, headers, method=method), timeout=30) as response:
            result = response.read()
        return result if binary else json.loads(result)

    def camera(member, value=None, binary=False):
        result = request("/api/v1/camera/0/" + member + "?ClientID=1", value, "GET" if value is None else "PUT", binary)
        if binary:
            return result
        assert result["ErrorNumber"] == 0, result
        return result.get("Value")

    with tempfile.TemporaryDirectory(prefix="regain-http-") as directory:
        with open(Path(directory) / "server.log", "w+b") as log:
            process = subprocess.Popen([str(executable), "--simulate", "--no-discovery", "--port", str(port),
                                        "--profiles", str(Path(directory) / "cameras.json")], stdout=log, stderr=log,
                                       creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
            try:
                deadline = time.monotonic() + 30
                while True:
                    try:
                        request("/setup/api/state")
                        break
                    except urllib.error.URLError:
                        assert process.poll() is None and time.monotonic() < deadline, "Server did not start"
                        time.sleep(0.05)
                for direct, selected in ((False, None), (True, None), (True, "ZWO ASI2600MM Pro")):
                    descriptors = request("/setup/api/discover", {"direct": direct})
                    descriptor = next(c for c in descriptors if c["name"] == selected) if selected else descriptors[0]
                    profile = request("/setup/api/state")["cameras"][0]["profile"]
                    profile.update(camera=descriptor, serial=None, direct=direct)
                    profile["recovery"]["reconnectDelaySeconds"] = 0.05
                    request("/setup/api/cameras/0", profile)
                    assert request("/setup/api/state")["cameras"][0]["profile"]["camera"]["name"] == descriptor["name"]
                    camera("connected", {"Connected": "true", "ClientID": 1})
                    if selected:
                        assert camera("cameraxsize") == 6248 and camera("cameraysize") == 4176
                        assert camera("maxbinx") == 4 and camera("exposuremax") == 2000
                    camera("numx", {"NumX": 64, "ClientID": 1})
                    camera("numy", {"NumY": 64, "ClientID": 1})
                    camera("startexposure", {"Duration": 0.01, "Light": "false", "ClientID": 1})
                    deadline = time.monotonic() + 30
                    while not camera("imageready"):
                        assert time.monotonic() < deadline, "Capture timed out"
                        time.sleep(0.02)
                    data = camera("imagearray", binary=True)
                    header = struct.unpack("<11I", data[:44])
                    assert header[0:2] == (1, 0) and header[4:] == (44, 2, 8, 2, 64, 64, 0), header
                    assert len(data) == 44 + 64 * 64 * 2
                    print(f"Standalone {'direct' if direct else 'SDK'} {descriptor['name']} ImageBytes capture passed")
                    camera("startexposure", {"Duration": 2, "Light": "false", "ClientID": 1})
                    camera("abortexposure", {"ClientID": 1})
                    assert camera("camerastate") == 0 and not camera("imageready")
                    camera("connected", {"Connected": "false", "ClientID": 1})
                if os.name != "nt":
                    camera("connected", {"Connected": "true", "ClientID": 1})
                    worker = json.loads(camera("action", {"Action": "Regain.Diagnostics", "Parameters": "", "ClientID": 1}))["processId"]
                    camera("startexposure", {"Duration": 2, "Light": "false", "ClientID": 1})
                    process.terminate()  # SIGTERM must abort capture and close its camera worker.
                    assert process.wait(timeout=30) == 0, "Unclean SIGTERM shutdown"
                    log.seek(0)
                    assert b"Disconnected" in log.read()
                    try:
                        os.kill(worker, 0)
                    except ProcessLookupError:
                        pass
                    else:
                        raise AssertionError("Camera worker survived server shutdown")
                    print("Graceful SIGTERM shutdown during capture passed")
            finally:
                if process.poll() is None:
                    process.kill()
                process.wait(timeout=30)


if __name__ == "__main__":
    main()
