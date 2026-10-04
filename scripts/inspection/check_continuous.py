"""Explicit operator-run continuous transition check. Never part of automated CI.
Requires --hardware; images stay in memory, output excludes camera identifiers.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("pipe_fixture", ROOT / "scripts/test-rust.py")
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hardware", action="store_true", required=True)
    parser.add_argument("--model", choices=["ZWO ASI662MC", "ZWO ASI676MC"], required=True)
    parser.add_argument("--backend", choices=["sdk", "direct"], required=True)
    parser.add_argument("--sdk", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--long", action="store_true")
    args = parser.parse_args()
    command = [str(ROOT / "target/debug/regain-device.exe"), "zwo", "camera-" + args.backend]
    if args.backend == "direct": command += ["--serve"]
    else:
        assert args.sdk and args.sdk.is_file()
        command += ["--sdk", str(args.sdk.resolve())]
    with args.output.open("x", encoding="utf-8") as output, fixture.Worker(command) as worker:
        worker.timer.cancel()
        worker.timer = threading.Timer(900, worker.process.kill)
        worker.timer.start()
        matches = [c for c in worker.call("list")[0] if c["name"] == args.model]
        assert len(matches) == 1, "ambiguous or missing selected model"
        selected = matches[0]
        identity = dict(name=args.model)
        if args.backend == "sdk": identity["id"] = selected["id"]
        else: identity["locator"] = selected["locator"]
        opened = worker.call("open", identity)[0]
        assert opened["continuousAcquisition"]["supported"]
        width, height = ((1920, 1080) if "662" in args.model else (3552, 3552))
        sequence = [234000, 900000, 1000000, 2000000, 6400000, 234000]
        if args.long: sequence = [6400000, 25000000, 25000000, 60000000, 234000]
        for index, exposure in enumerate(sequence):
            began = time.monotonic()
            params = dict(width=width, height=height, bin=1, x=0, y=0,
                          microseconds=exposure, gain=300 if index % 2 == 0 else 270,
                          dark=False, maxFps=0.5)
            worker.call("stream-start", params)
            frames = []
            deadline = began + 4 * max(exposure, max(sequence[:index], default=0)) / 1e6 + 60
            while len(frames) < 2:
                status = worker.call("stream-status")[0]
                assert status["error"] is None, status["error"]
                assert time.monotonic() < deadline, "transition/frame deadline expired"
                if status["ready"] and not status["settingsPending"] and not status["settling"]:
                    meta, pixels = worker.call("stream-download")
                    assert len(pixels) == width * height * 2
                    frames.append(dict(sequence=meta["acquisitionSequence"],
                                       generation=meta["settingsGeneration"],
                                       seconds=round(time.monotonic() - began, 3)))
                else: time.sleep(0.02)
            assert frames[1]["sequence"] > frames[0]["sequence"]
            assert frames[1]["seconds"] - frames[0]["seconds"] >= 1.9
            record = dict(model=args.model, backend=args.backend, exposure=exposure,
                          gain=params["gain"], mode=status["mode"], frames=frames,
                          acquired=status["acquiredFrames"], replaced=status["replacedFrames"])
            output.write(json.dumps(record) + "\n")
            output.flush()
            print(json.dumps(record), flush=True)
        worker.call("stream-stop")
        deadline = time.monotonic() + 90
        while worker.call("stream-status")[0]["active"]:
            assert time.monotonic() < deadline
            time.sleep(0.1)
        worker.call("close")


if __name__ == "__main__": main()
