"""Exercise the Rust workers over real pipes, without a camera or vendor SDK."""
import argparse
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import threading
import time


class Worker:
    def __init__(self, command, *, env=None):
        self.log = tempfile.TemporaryFile()
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=self.log, env=env)
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
    with Worker([str(binary_dir / ("regain-device" + suffix)), "zwo", "camera-sdk", "--simulate"]) as worker:
        name = worker.call("list")[0][0]["name"]
        worker.call("open", dict(name=name))
        worker.call("list", error=True)
        assert worker.frame(exposure)[1] == expected
        worker.call("list", error=True)
        worker.call("close")
        assert worker.call("list")[0][0]["name"] == name
    count = 0
    with Worker([str(binary_dir / ("regain-device" + suffix)), "zwo", "camera-direct", "--serve", "--simulate"]) as worker:
        cameras = worker.call("list")[0]
        assert len(cameras) == 6
        for camera in cameras:
            opened = worker.call("open", dict(name=camera["name"]))[0]
            if camera["name"] in ["ZWO ASI676MC", "ZWO ASI662MC"]:
                exposure_cap = next(c for c in opened['controls'] if c['type'] == 1)
                assert exposure_cap['min'] == 32 and exposure_cap['max'] == 2000000000
                for duration in [30000001, 60000000, 120000000, 2000000000]:
                    worker.call('validate', dict(exposure, microseconds=duration))
                    worker.call('set', dict(control=1, value=duration))
                    assert worker.call('get', dict(control=1))[0] == duration
                worker.call('validate', dict(exposure, microseconds=2000000001), error=True)
                worker.call('set', dict(control=1, value=2000000001), error=True)
            if camera["name"] == "ZWO ASI662MC":
                assert camera['color'] and camera['bayer'] == 0 and camera['bitDepth'] == 12
                assert camera['width'] == 1920 and camera['height'] == 1080 and camera['pixelSize'] == 2.9
                assert camera['bins'] == [1] and camera['originAlignment'] == 8 and camera['retainedFrameReads']
                worker.call('set', dict(control=5, value=300))
                worker.call('set', dict(control=5, value=301), error=True)
                worker.call('validate', dict(exposure, bin=1, x=16, y=8))
                worker.call('validate', dict(exposure, bin=1, x=16, y=2), error=True)
                worker.call('validate', dict(exposure, bin=2), error=True)
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
    with Worker([str(binary_dir / "regain-device"), "zwo", "camera-sdk", "--sdk", str(library.resolve())]) as worker:
        camera = worker.call("list")[0][0]
        assert camera["width"] == 9576 and camera["height"] == 6388
        assert camera["pixelSize"] == 3.76 and camera["bitDepth"] == 16
        result = worker.call("open", dict(name=camera["name"]))[0]
        worker.call("list", error=True)
        assert result["sdkVersion"] == "C ABI fixture"
        assert result["controls"][0]["min"] == -123
        worker.call("set", dict(control=0, value=-42))
        assert worker.call("get", dict(control=0))[0] == -42
        worker.call("set", dict(control=0, value=1 << 40))
        assert worker.call("get", dict(control=0))[0] == 1 << 40
        worker.call("close")
        assert worker.call("list")[0][0]["name"] == camera["name"]
    print("Passed: native SDK loading and C header ABI fixture")
    standalone(binary_dir, library)


def white_balance(binary_dir):
    """Both public worker protocols, strictly simulated: no SDK load or USB."""
    suffix = ".exe" if sys.platform == "win32" else ""
    binary = str(binary_dir / ("regain-device" + suffix))
    e = dict(width=128, height=128, x=0, y=0, bin=1, microseconds=1000, dark=False)
    outputs = []
    for backend, extra, name in [("camera-sdk", [], "ZWO Simulated"),
                                  ("camera-direct", ["--serve"], "ZWO ASI662MC")]:
        with Worker([binary, "zwo", backend, *extra, "--simulate"]) as w:
            w.call("white-balance", error=True)
            opened, _ = w.call("open", dict(name=name))
            assert opened["whiteBalance"]["supported"]
            assert w.call("white-balance")[0]["managed"] is False
            legacy_meta, raw = w.frame(e)
            assert "whiteBalance" not in legacy_meta
            manual = dict(mode="manual", gains=dict(red=2.0, blue=0.5), output="raw")
            for invalid in [dict(mode="bad"), dict(mode="manual", unexpected=True),
                            dict(mode="manual", gains=dict(red=0, blue=1)),
                            dict(mode="manual", gains=dict(red=9, blue=1))]:
                w.call("white-balance", invalid, error=True)
                assert w.call("white-balance")[0]["managed"] is False
            w.call("white-balance", manual)
            meta, pixels = w.frame(e)
            assert pixels == raw and meta["whiteBalance"]["applied"] is False
            w.call("white-balance", {**manual, "output": "corrected"})
            # Native SDK WB and flip cannot be mixed with managed WB.
            for control in [3, 4, 9]:
                w.call("set", dict(control=control, value=50), error=True)
                if backend == "camera-sdk":
                    w.call("set-control-state", dict(control=control, value=50, auto=True), error=True)
            w.call("start", e)
            w.call("white-balance", dict(mode="off"), error=True)
            while w.call("status")[0] != 2:
                time.sleep(0.01)
            meta, pixels = w.call("download")
            assert meta["whiteBalance"]["applied"] is True
            expected = bytearray()
            for i, (value,) in enumerate(struct.iter_unpack("<H", raw)):
                x, y = i % 128, i // 128
                gain = 2 if x % 2 == y % 2 == 0 else 0.5 if x % 2 == y % 2 == 1 else 1
                expected.extend(struct.pack("<H", min(65535, int(value * gain + 0.5))))
            assert pixels == expected
            outputs.append(pixels)
            meta, pixels = w.frame({**e, "dark": True})
            assert pixels == raw and meta["whiteBalance"]["applied"] is False
            w.call("white-balance", dict(mode="once"))
            meta, pixels = w.frame(e)
            assert pixels == raw
            assert meta["whiteBalance"]["estimation"] == "updated"
            assert w.call("white-balance")[0]["settings"]["mode"] == "locked"
            w.call("white-balance", dict(mode="continuous"))
            w.frame(e)
            gains = w.call("white-balance")[0]["settings"]["gains"]
            locked, _ = w.call("white-balance", dict(mode="locked"))
            assert locked["settings"]["gains"] == gains
            w.call("start", {**e, "bin": 2}, error=True)
            w.call("white-balance", dict(mode="off", output="corrected"))
            assert w.frame(e)[1] == raw
            w.call("close")
            w.call("open", dict(name=name))
            assert w.call("white-balance")[0]["managed"] is False
            if backend == "camera-sdk":
                w.call("set", dict(control=9, value=1))
                w.call("white-balance", dict(mode="once"), error=True)
                assert w.call("white-balance")[0]["managed"] is False
                w.call("set", dict(control=9, value=0))
            w.call("close")
            if backend == "camera-sdk":
                w.call("simulation", dict(color=False))
                mono = name
            else:
                mono = "ZWO ASI220MM Mini"
            assert w.call("open", dict(name=mono))[0]["whiteBalance"]["supported"] is False
            w.call("white-balance", dict(mode="once"), error=True)
            w.call("close")
    assert outputs[0] == outputs[1], "SDK and Direct applied different WB processing"
    print("Passed: SDK/Direct shared WB, raw preservation, AWB once/continuous/lock, validation and lifecycle")


def white_balance_fixture(binary_dir):
    """Build our own inert ABI fixture; no installed/vendor SDK is loaded."""
    suffix = ".exe" if sys.platform == "win32" else ""
    library_suffix = ".dll" if sys.platform == "win32" else ".dylib" if sys.platform == "darwin" else ".so"
    source = Path(__file__).resolve().parent.parent / "tests/fixtures/white_balance_sdk.rs"
    with tempfile.TemporaryDirectory(prefix="regain-wb-fixture-") as directory:
        library = Path(directory) / ("white_balance_sdk" + library_suffix)
        subprocess.run(["rustc", "--edition", "2021", "--crate-type", "cdylib", str(source), "-o", str(library)], check=True, timeout=120)
        command = [str(binary_dir / ("regain-device" + suffix)), "zwo", "camera-sdk", "--sdk", str(library)]
        e = dict(width=64, height=64, x=0, y=0, bin=1, microseconds=1000, dark=False)
        for fault in [None, "REGAIN_FIXTURE_REJECT_WB", "REGAIN_FIXTURE_RESTORE_FAIL"]:
            env = dict(os.environ)
            if fault:
                env[fault] = "1"
            with Worker(command, env=env) as w:
                assert w.call("open", dict(name="WB fixture"))[0]["whiteBalance"]["supported"]
                saved = [w.call("get-control-state", dict(control=c))[0] for c in [3, 4]]
                w.call("white-balance", dict(mode="once", output="corrected"))
                if fault == "REGAIN_FIXTURE_REJECT_WB":
                    w.call("start", e, error=True)
                    assert w.call("status")[0] == 0
                else:
                    meta, pixels = w.frame(e)
                    assert meta["whiteBalance"]["settings"]["gains"] == dict(red=2.0, blue=0.5)
                    assert pixels == struct.pack("<H", 8000) * (64 * 64)
                    for c in [3, 4]:
                        assert w.call("get-control-state", dict(control=c))[0] == dict(value=50, auto=False)
                w.call("close", error=fault == "REGAIN_FIXTURE_RESTORE_FAIL")
                if fault != "REGAIN_FIXTURE_RESTORE_FAIL":
                    w.call("open", dict(name="WB fixture"))
                    assert [w.call("get-control-state", dict(control=c))[0] for c in [3, 4]] == saved
                    w.call("close")
    print("Passed: synthetic SDK ABI neutralization, AWB, restore including auto flags, and fail-closed readback/restore errors")


def standalone(binary_dir, library=None):
    suffix = ".exe" if sys.platform == "win32" else ""
    command = [str(binary_dir / ("regain-device" + suffix)), "zwo", "camera-sdk"]
    command += ["--sdk", str(library.resolve())] if library else ["--simulate"]
    listing = json.loads(subprocess.check_output(command + ["--list"], text=True, timeout=10))
    assert len(listing["cameras"]) == 1
    with tempfile.TemporaryDirectory(prefix="regain-cli-test-") as temporary:
        destination = Path(temporary) / "frames"
        capture = command + ["--capture", "--width", "128", "--height", "128", "--frames", "2",
                             "--microseconds", "1000", "--gain", "123", "--output", str(destination)]
        done = subprocess.run(capture, capture_output=True, text=True, timeout=20, check=True)
        assert len(done.stdout.splitlines()) == 2
        expected = b"".join(struct.pack("<H", n) for n in range(128 * 128))
        for index in [1, 2]:
            assert (destination / f"frame-{index:04}.raw").read_bytes() == expected
            metadata = json.loads((destination / f"frame-{index:04}.json").read_text())
            assert metadata["exposure"]["dark"] is True and metadata["readRetriesUsed"] == 0
        # Do not overwrite an existing output directory or its images.
        assert subprocess.run(capture, capture_output=True, timeout=10).returncode != 0
        assert (destination / "frame-0001.raw").read_bytes() == expected
        if library:
            sets = [line for line in done.stderr.splitlines() if line.startswith("fixture set 0=")]
            assert sets == ["fixture set 0=123", "fixture set 0=100"], sets
            for failures in [2, 3]:
                retry_directory = Path(temporary) / f"retry-{failures}"
                retried = subprocess.run(capture[:-1] + [str(retry_directory)],
                    env={**os.environ, "REGAIN_FIXTURE_FAIL_DOWNLOADS": str(failures)},
                    capture_output=True, text=True, timeout=20)
                assert (retried.returncode == 0) == (failures == 2), retried.stderr
                assert "fixture set 0=100" in retried.stderr, "gain not restored after failed download"
                if failures == 2:
                    metadata = json.loads((retry_directory / "frame-0001.json").read_text())
                    assert metadata["readRetriesUsed"] == 2
                    assert (retry_directory / "frame-0001.raw").read_bytes() == expected
                else:
                    assert not list(retry_directory.glob("*.raw")), "accepted a failed frame"
    print("Passed: standalone SDK capture, RAW16 output, and settings restoration" if library
          else "Passed: standalone simulated camera commands")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, default=Path("target/debug"))
    parser.add_argument("--sdk-fixture", type=Path)
    args = parser.parse_args()
    simulated(args.bin_dir.resolve())
    white_balance(args.bin_dir.resolve())
    white_balance_fixture(args.bin_dir.resolve())
    standalone(args.bin_dir.resolve())
    if args.sdk_fixture:
        sdk_fixture(args.bin_dir.resolve(), args.sdk_fixture)
