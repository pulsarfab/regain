"""Exercise the Rust workers over real pipes, without a camera or vendor SDK."""
import argparse
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import threading
import time


class Worker:
    def __init__(self, command):
        self.log = tempfile.TemporaryFile()
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=self.log)
        self.timer = threading.Timer(30, self.process.kill)
        self.timer.start()
        self.sequence = 0

    def __enter__(self):
        return self

    def __exit__(self, kind, value, traceback):
        try:
            self.process.stdin.close()
            self.process.wait(timeout=3)
            if kind is None:
                assert self.process.returncode == 0, self.process.returncode
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait()
            self.timer.cancel()
            self.process.stdout.close()
            if kind is not None or self.process.returncode:
                self.log.seek(0)
                print(self.log.read().decode(errors="replace"))
            self.log.close()

    def read(self, size):
        data = bytearray()
        while len(data) < size:
            chunk = self.process.stdout.read(size - len(data))
            assert chunk, "worker ended before completing its reply"
            data.extend(chunk)
        return bytes(data)

    def call(self, method, params=None, *, error=False):
        self.sequence += 1
        request = json.dumps(dict(version=1, id=self.sequence, method=method,
                                  params=params or {})).encode()
        self.process.stdin.write(struct.pack("<I", len(request)) + request)
        self.process.stdin.flush()
        length, = struct.unpack("<I", self.read(4))
        assert 0 < length <= 65536
        reply = json.loads(self.read(length))
        assert reply["version"] == 1 and reply["id"] == self.sequence
        assert reply["ok"] is not error, reply
        size = reply["binaryLength"]
        assert 0 <= size <= 512 * 1024 * 1024
        pixels = self.read(size)
        return reply.get("result"), pixels

    def frame(self, exposure):
        self.call("start", exposure)
        deadline = time.monotonic() + 5
        while self.call("status")[0] != 2:
            assert time.monotonic() < deadline, "capture did not finish"
            time.sleep(0.01)
        metadata, pixels = self.call("download")
        assert len(pixels) == exposure["width"] * exposure["height"] * 2
        return metadata, pixels


def simulated(binary_dir):
    suffix = ".exe" if sys.platform == "win32" else ""
    exposure = dict(width=128, height=128, x=0, y=0, bin=1,
                    microseconds=1000, dark=True, readRetries=2)
    expected = b"".join(struct.pack("<H", n) for n in range(128 * 128))
    with Worker([str(binary_dir / ("zwogain-host" + suffix)), "--simulate"]) as worker:
        name = worker.call("list")[0][0]["name"]
        worker.call("open", dict(name=name))
        assert worker.frame(exposure)[1] == expected
        worker.call("close")
    count = 0
    with Worker([str(binary_dir / ("zwogain-direct" + suffix)), "--serve", "--simulate"]) as worker:
        cameras = worker.call("list")[0]
        assert len(cameras) == 4
        for camera in cameras:
            worker.call("open", dict(name=camera["name"]))
            if camera["cooled"]:
                for control, value in [(16, -10), (17, 1), (21, 1)]:
                    worker.call("set", dict(control=control, value=value))
                    assert worker.call("get", dict(control=control))[0] == value
            for binning in camera["bins"]:
                exposure["bin"] = binning
                metadata, pixels = worker.frame(exposure)
                assert metadata["sdkLoaded"] is False and pixels == expected
                count += 1
            worker.call("simulate-read-failures", dict(count=2))
            metadata, _ = worker.frame(exposure)
            assert metadata["readRecoveries"] == 2
            worker.call("simulate-read-failures", dict(count=3))
            worker.call("start", exposure)
            worker.call("status", error=True)
            worker.call("download", error=True)
            worker.call("close")
        worker.call("open", dict(name=cameras[0]["name"]))
        worker.call("simulation", dict(cleanupFailure=True))
        exposure["bin"] = 1
        metadata, pixels = worker.frame(exposure)
        assert "cleanupError" in metadata and pixels == expected
        worker.call("start", exposure, error=True)
        worker.call("close")
    print(f"Passed: SDK simulator, {count} direct camera/bin captures, cooler controls, read retries, cleanup recovery")


def sdk_fixture(binary_dir, library):
    with Worker([str(binary_dir / "zwogain-host"), "--sdk", str(library.resolve())]) as worker:
        camera = worker.call("list")[0][0]
        assert camera["width"] == 9576 and camera["height"] == 6388
        assert camera["pixelSize"] == 3.76 and camera["bitDepth"] == 16
        result = worker.call("open", dict(name=camera["name"]))[0]
        assert result["sdkVersion"] == "C ABI fixture"
        assert result["controls"][0]["min"] == -123
        worker.call("set", dict(control=0, value=-42))
        assert worker.call("get", dict(control=0))[0] == -42
        worker.call("close")
    print("Passed: native SDK loading and C header ABI fixture")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, default=Path("target/debug"))
    parser.add_argument("--sdk-fixture", type=Path)
    args = parser.parse_args()
    simulated(args.bin_dir.resolve())
    if args.sdk_fixture:
        sdk_fixture(args.bin_dir.resolve(), args.sdk_fixture)
