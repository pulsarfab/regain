"""Owned-camera full-frame thermal/control acceptance, with passive USB tracing.

Requires Windows, Frida and an idle, authorized ASI6200MM Pro. Every exposure
uses the entire 9576x6388 RAW16 sensor at bin 1; there is no ROI/short-test mode.
Runs direct and SDK workers serially, pins the camera serial, changes cooling
and supported dew heating, and applies logging/power-limit options live. No
fault injection, process replacement, USB reset or external app control occurs.
Restores original writable settings; disconnect deliberately leaves cooler and
heater off. Private traces contain device identities; do not publish them.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import queue
import subprocess
import threading
import time
import traceback

from validate_6200_abort import (Worker, exposure, pixel_statistics, require,
                                verify_thermal_readback)


ROOT = Path(__file__).resolve().parents[2]
MODEL = "ZWO ASI6200MM Pro"


class ControlWorker(Worker):
    """Reuse the bounded framed-protocol readers and passive trace helpers."""

    def __init__(self, executable, sdk, backend, output):
        import frida
        self.output, self.sequence = output, 0
        self.events, self.trace_events = [], []
        self.condition = threading.Condition()
        self.replies = queue.Queue()
        self.trace_lock = threading.Lock()
        self.log = (output / "worker.log").open("x", encoding="utf-8")
        self.trace = (output / "private-usb.jsonl").open("x", encoding="utf-8")
        command = [str(executable), "zwo", "camera-" + backend]
        command += ["--serve"] if backend == "direct" else ["--sdk", str(sdk)]
        environment = dict(os.environ, REGAIN_CAMERA_TRANSFER_LOG_MODE="circular")
        # A host-wide forced-off override would defeat the live-mode check.
        environment.pop("REGAIN_CAMERA_TRANSFER_LOG", None)
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        env=environment)
        self.pid, self.session, self.script = self.process.pid, None, None
        self.stdout_thread = threading.Thread(target=self._stdout, daemon=True)
        self.stderr_thread = threading.Thread(target=self._stderr, daemon=True)
        self.stdout_thread.start()
        self.stderr_thread.start()
        try:
            self.session = frida.get_local_device().attach(self.pid)
            source = Path(__file__).with_name("trace-transport.js").read_text(encoding="utf-8")
            self.script = self.session.create_script(source)
            self.script.on("message", self._trace)
            self.script.load()
            self.record_trace(dict(kind="acceptance-worker", pid=self.pid, backend=backend,
                                   sha256=hashlib.sha256(executable.read_bytes()).hexdigest()))
        except BaseException:
            self.finish()
            raise

    def settle_trace(self):
        """Drain asynchronous observer delivery without issuing USB commands."""
        deadline, unchanged, previous = time.monotonic() + 1, time.monotonic(), -1
        while time.monotonic() < deadline:
            count = len(self.trace_events)
            if count != previous:
                previous, unchanged = count, time.monotonic()
            elif time.monotonic() - unchanged >= .1:
                break
            time.sleep(.02)


def writes(events, request, register=None):
    """Successful OUT commands only; submits/completions are not double-counted."""
    result, completed = [], set()
    for event in events:
        if event.get("kind") not in ("io-return", "io-complete") or not event.get("ok"):
            continue
        if event["sequence"] in completed:
            continue
        header = bytes.fromhex(event.get("header") or "")
        if len(header) < 8 or header[:2] != bytes([0x40, request]):
            continue
        if len(header) >= 22 and (int.from_bytes(header[14:18], "little") or
                                  int.from_bytes(header[18:22], "little")):
            continue
        value = int.from_bytes(header[2:4], "little")
        if register is not None and value != register:
            continue
        completed.add(event["sequence"])
        result.append(dict(sequence=event["sequence"], timeMs=event.get("timeMs"),
                           value=value, data=int.from_bytes(header[4:6], "little")))
    return result


def summarize_window(events):
    power = writes(events, 0xbd, 0x26)
    repeated_gaps = [b["timeMs"]-a["timeMs"] for a, b in zip(power, power[1:])
                     if a["data"] == b["data"]]
    changed_gaps = [b["timeMs"]-a["timeMs"] for a, b in zip(power, power[1:])
                    if a["data"] != b["data"]]
    feedback = [e["timeMs"] for e in events if e.get("kind") == "io-submit" and
                (e.get("header") or "").startswith("c0b3")]
    return dict(coolerWrites=len(power),
                unchangedCoolerWrites=len(repeated_gaps),
                unchangedCoolerWriteGapsMs=repeated_gaps,
                changedCoolerWriteGapsMs=changed_gaps,
                coolerCommands=power, heaterWrites=len(writes(events, 0xbd, 0x2a)),
                flagWrites=len(writes(events, 0xbd, 0x19)),
                temperatureReads=len(feedback),
                maximumTemperatureReadGapMs=max((b-a for a, b in zip(feedback, feedback[1:])),
                                               default=None),
                initializationCommands=len(writes(events, 0xaf)),
                exposureStarts=len(writes(events, 0xa9)),
                handleOpens=sum(e.get("kind") == "device-open" for e in events))


def sample(worker, row, label):
    value = dict(label=label, atMonotonic=time.monotonic(),
                 temperatureC=worker.get(8) / 10, powerPercent=worker.get(15),
                 targetC=worker.get(16), coolerEnabled=worker.get(17))
    require(all(math.isfinite(value[k]) for k in ("temperatureC", "powerPercent")),
            "invalid cooler telemetry")
    row["samples"].append(value)
    return value


def frame(worker, row, backend, label, live=None):
    before = len(worker.trace_events)
    started = time.monotonic()
    options = exposure(5)
    worker.call("start", options)
    updated, next_sample = False, started
    while True:
        state = worker.call("status")[0]
        require(state in (1, 2), f"unexpected capture state {state}")
        if state == 2:
            break
        if live is not None and not updated and time.monotonic() - started >= 1:
            acknowledged = worker.call("runtime-options", live)[0]
            require(acknowledged == live, "live options acknowledgement changed values")
            updated = True
        if time.monotonic() >= next_sample:
            sample(worker, row, label)
            next_sample = time.monotonic() + 1
        require(time.monotonic() - started < 45, "full-frame acquisition deadline")
        time.sleep(.05)
    ready = time.monotonic()
    metadata, pixels = worker.call("download", timeout=45)
    delivered = time.monotonic()
    require(len(pixels) == 9576 * 6388 * 2, "full RAW16 frame byte count mismatch")
    require(metadata.get("width") == 9576 and metadata.get("height") == 6388,
            "frame metadata does not describe the whole sensor")
    digest = hashlib.sha256(pixels).hexdigest()
    if backend == "direct":
        require(metadata.get("sdkLoaded") is False, "direct capture loaded the SDK")
        require(metadata.get("sha256") == digest, "worker frame digest mismatch")
        require(metadata.get("readRecoveries") == 0 and metadata.get("handleReopens", 0) == 0,
                "healthy capture unexpectedly needed recovery")
        require("cleanupError" not in metadata, "capture cleanup failed")
    require(live is None or updated, "capture finished before live update was exercised")
    statistics = pixel_statistics(pixels, 9576, 6388)
    del pixels
    worker.settle_trace()
    summary = summarize_window(worker.trace_events[before:])
    require(summary["exposureStarts"] == 1, "full capture did not have exactly one physical start")
    result = dict(label=label, readyAfterSeconds=ready-started,
                  pixelsAfterSeconds=delivered-started, metadata=metadata,
                  sha256=digest, statistics=statistics, usb=summary, liveOptions=live)
    row["frames"].append(result)
    print(json.dumps(dict(backend=backend, frame=label, seconds=delivered-started,
                          power=worker.get(15), temperatureC=worker.get(8)/10)), flush=True)


def same_requests(worker, row, label, controls):
    worker.settle_trace()
    before = len(worker.trace_events)
    start = time.monotonic()
    for _ in range(3):
        for control in controls:
            value = worker.get(control)
            worker.set(control, value)
    worker.settle_trace()
    row["sameRequests"].append(dict(label=label, seconds=time.monotonic()-start,
                                    controls=controls,
                                    usb=summarize_window(worker.trace_events[before:])))


def run_backend(args, backend, report):
    output = args.output / backend
    output.mkdir()
    row = dict(backend=backend, passed=False, frames=[], samples=[], sameRequests=[], cleanup=[])
    report["backends"].append(row)
    worker, opened, initial = None, False, {}
    try:
        worker = ControlWorker(args.worker, args.sdk, backend, output)
        row["pid"] = worker.pid
        cameras = worker.call("list", dict(serials=True))[0]
        selected = [c for c in cameras if c.get("name") == MODEL and
                    str(c.get("serial", "")).lower() == args.serial]
        require(len(selected) == 1, "exact serial is not uniquely available")
        identity = worker.call("open", dict(name=MODEL, serial=args.serial))[0]
        opened = True
        require(identity["serial"].lower() == args.serial, "opened camera serial mismatch")
        require(identity["info"]["width"] == 9576 and identity["info"]["height"] == 6388,
                "camera is not the expected full-frame model")
        row["identity"] = identity
        available = {c["type"] for c in identity["controls"]}
        initial = {c: worker.get(c) for c in (0, 5, 16, 17, 21, 22, 23) if c in available}
        row["initialControls"] = initial
        worker.call("runtime-options", dict(transferLogMode="circular",
                                             coolerMaxPowerChangePer5Seconds=25))
        worker.set(17, 0)
        if 21 in available:
            worker.set(21, 0)
        worker.set(0, 100)
        worker.set(5, 50)
        same_requests(worker, row, "disabled", [16, 17] + ([21] if 21 in available else []))
        for index in range(3):
            frame(worker, row, backend, f"disabled-{index+1}")
        # First acquisition may initialize/reapply. From here every healthy
        # capture must reuse the same transport and initialized sensor.
        worker.settle_trace()
        healthy_start = len(worker.trace_events)
        ambient = worker.get(8) / 10
        target = max(-5, min(20, math.floor(ambient - 5)))
        row.update(ambientC=ambient, testTargetC=target)
        worker.set(16, target)
        worker.set(17, 1)
        if 21 in available:
            worker.set(21, 1)
        for index in range(3):
            frame(worker, row, backend, f"cooldown-{index+1}")
        row["cooldown"] = summarize_window(worker.trace_events[healthy_start:])
        if backend == "direct":
            require(any(0 < gap < 4990 for gap in row["cooldown"]["changedCoolerWriteGapsMs"]),
                    "changed cooler output was delayed by the unchanged-output keepalive")
        started, stable = time.monotonic(), 0
        while stable < 5:
            value = sample(worker, row, "convergence")
            stable = stable+1 if abs(value["temperatureC"] - target) <= 1 else 0
            require(time.monotonic()-started < args.convergence_seconds,
                    "camera did not converge within the thermal acceptance budget")
            time.sleep(1)
        row["convergenceSecondsAfterCooldownFrames"] = time.monotonic()-started
        worker.settle_trace()
        hold_start, held_at = len(worker.trace_events), time.monotonic()
        same_requests(worker, row, "enabled", [16, 17] + ([21] if 21 in available else []))
        for index in range(3):
            frame(worker, row, backend, f"hold-{index+1}")
        while time.monotonic()-held_at < 30:
            sample(worker, row, "hold")
            time.sleep(1)
        worker.settle_trace()
        row["steadyHold"] = summarize_window(worker.trace_events[hold_start:])
        row["steadyHold"]["seconds"] = time.monotonic()-held_at
        require(row["steadyHold"]["temperatureReads"] >= 15,
                "unchanged output must not stop temperature feedback")
        if backend == "direct":
            require(all(gap >= 4990 for gap in
                        row["steadyHold"]["unchangedCoolerWriteGapsMs"]),
                    "direct controller repeated an unchanged cooler output before keepalive")
            enabled_requests = row["sameRequests"][-1]["usb"]
            require(enabled_requests["flagWrites"] == 0 and enabled_requests["heaterWrites"] == 0,
                    "unchanged enabled cooler/heater requests wrote actuator flags")
        # Setpoint reversal and runtime updates exercise the same sensor,
        # including off logging and its later reenable during an acquisition.
        worker.set(16, min(30, target+2))
        for mode, limit in (("verbose", 10), ("off", 100), ("circular", 25)):
            frame(worker, row, backend, f"live-{mode}-{limit}",
                  dict(transferLogMode=mode, coolerMaxPowerChangePer5Seconds=limit))
        worker.set(16, target)
        frame(worker, row, backend, "colder-setpoint")
        if backend == "direct":
            # With cooling still enabled, a target above the sensor temperature
            # gives a known unchanged zero-demand output. Keep full-frame
            # acquisitions running throughout the dedicated keepalive window.
            worker.set(16, 30)
            worker.call("runtime-options", dict(transferLogMode="circular",
                                                 coolerMaxPowerChangePer5Seconds=100))
            settling_started, settling_frames = time.monotonic(), 0
            while worker.get(15) != 0:
                require(time.monotonic()-settling_started < 45,
                        "zero-demand keepalive precondition did not converge")
                settling_frames += 1
                frame(worker, row, backend, f"keepalive-settling-{settling_frames}")
            require(worker.get(17) == 1 and worker.get(8) / 10 < 29,
                    "keepalive requires enabled cooling with a warmer target")
            worker.settle_trace()
            refresh_start, refreshing_at, refresh_frames = len(worker.trace_events), time.monotonic(), 0
            refreshing_unix_ms = time.time()*1000
            while time.monotonic()-refreshing_at < 30:
                refresh_frames += 1
                frame(worker, row, backend, f"keepalive-{refresh_frames}")
            worker.settle_trace()
            keepalive = summarize_window(worker.trace_events[refresh_start:])
            keepalive["seconds"] = time.monotonic()-refreshing_at
            keepalive["windowStartUnixMs"] = refreshing_unix_ms
            keepalive["windowEndUnixMs"] = time.time()*1000
            require(keepalive["coolerWrites"] >= 4,
                    "unchanged cooler output was not periodically refreshed")
            require(all(command["data"] == 14 for command in keepalive["coolerCommands"]),
                    "keepalive phase did not retain the zero-demand FPGA output")
            require(all(4990 <= gap <= 8000 for gap in
                        keepalive["unchangedCoolerWriteGapsMs"]),
                    "unchanged cooler refresh cadence fell outside the service budget")
            keepalive["firstRefreshGapMs"] = keepalive["coolerCommands"][0]["timeMs"]-refreshing_unix_ms
            keepalive["lastRefreshGapMs"] = keepalive["windowEndUnixMs"]-keepalive["coolerCommands"][-1]["timeMs"]
            require(all(0 <= keepalive[key] <= 8000 for key in
                        ("firstRefreshGapMs", "lastRefreshGapMs")),
                    "keepalive did not span the entire observation window")
            # Required capture clock transitions also write BD19. They must
            # preserve thermal bits; they are not repeated cooler commands.
            thermal_flags = 0x40 if 21 in available else 0
            require(all(command["data"] & 0xc0 == thermal_flags for command in
                        writes(worker.trace_events[refresh_start:], 0xbd, 0x19)) and
                    keepalive["heaterWrites"] == 0,
                    "keepalive captures changed cooler/heater flags or the heater preset")
            require(keepalive["temperatureReads"] >= 25,
                    "keepalive starved temperature feedback")
            require(keepalive["maximumTemperatureReadGapMs"] <= 3500,
                    "temperature feedback was starved during keepalive captures")
            row["unchangedOutputKeepalive"] = keepalive
        worker.set(17, 0)
        if 21 in available:
            worker.set(21, 0)
        same_requests(worker, row, "disabled-after-cooling", [17] + ([21] if 21 in available else []))
        frame(worker, row, backend, "after-off")
        require(worker.get(15) == 0 and worker.get(17) == 0, "cooler-off was not immediate")
        worker.settle_trace()
        row["healthyReuse"] = summarize_window(worker.trace_events[healthy_start:])
        require(row["healthyReuse"]["handleOpens"] == 0, "healthy test reopened the USB transport")
        if backend == "direct":
            require(row["healthyReuse"]["initializationCommands"] == 0,
                    "healthy test reinitialized the sensor")
        require(len({f["sha256"] for f in row["frames"]}) == len(row["frames"]),
                "consecutive healthy captures returned an identical whole frame")
        row["passed"] = True
    except BaseException:
        row["error"] = traceback.format_exc()
        raise
    finally:
        if worker is not None:
            try:
                if opened:
                    # Always attempt thermal shutdown even when another
                    # restoration command fails; every outcome is retained.
                    try:
                        worker.call("stop")
                        row["cleanup"].append(dict(stop=True, ok=True))
                    except Exception as error:
                        row["cleanup"].append(dict(stop=True, ok=False, error=str(error)))
                    restoration = [(17, 0)] + ([(21, 0)] if 21 in initial else [])
                    restoration += [(c, initial[c]) for c in (0, 5, 16, 22, 23) if c in initial]
                    for control, value in restoration:
                        try:
                            worker.set(control, value)
                            row["cleanup"].append(dict(control=control, value=value, ok=True))
                        except Exception as error:
                            row["cleanup"].append(dict(control=control, value=value, ok=False,
                                                       error=str(error)))
                    try:
                        worker.call("close")
                        row["cleanup"].append(dict(close=True, ok=True))
                        physical = worker.physical_thermal(0)
                        verify_thermal_readback(physical, 0x80, 0xc0 if 21 in initial else 0x80)
                        row["shutdownReadback"] = physical
                        row["cleanup"].append(dict(physicalThermalOff=True, ok=True))
                    except Exception as error:
                        row["cleanup"].append(dict(closeOrReadback=True, ok=False, error=str(error)))
            finally:
                worker.finish()
                row["workerExitCode"] = worker.process.returncode
                row["diagnosticEventCounts"] = {
                    name: sum(e.get("event") == name for e in worker.events)
                    for name in sorted({e.get("event") for e in worker.events if e.get("event")})}
                row["passed"] = row["passed"] and all(c["ok"] for c in row["cleanup"]) \
                    and worker.process.returncode == 0
        (args.output / "results.json").write_text(json.dumps(report, indent=2)+"\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hardware", action="store_true", required=True)
    parser.add_argument("--worker", type=Path, required=True)
    parser.add_argument("--sdk", type=Path, default=ROOT / "vendor/zwo/ASICamera2.dll")
    parser.add_argument("--serial", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--convergence-seconds", type=int, default=180)
    args = parser.parse_args()
    args.serial = args.serial.lower()
    if len(args.serial) != 16 or any(c not in "0123456789abcdef" for c in args.serial) or int(args.serial, 16) == 0:
        parser.error("--serial must be an exact nonzero 16-digit serial")
    if not 30 <= args.convergence_seconds <= 600:
        parser.error("--convergence-seconds must be 30..600")
    args.worker = args.worker.resolve(strict=True)
    args.sdk = args.sdk.resolve(strict=True)
    args.output.mkdir(parents=True, exist_ok=False)
    report = dict(camera=MODEL, serial=args.serial, backends=[], passed=False,
                  workerSha256=hashlib.sha256(args.worker.read_bytes()).hexdigest(),
                  sdkSha256=hashlib.sha256(args.sdk.read_bytes()).hexdigest(),
                  scriptSha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest())
    for backend in ("direct", "sdk"):
        require(hashlib.sha256(args.worker.read_bytes()).hexdigest() == report["workerSha256"],
                "worker changed during physical acceptance")
        run_backend(args, backend, report)
        require(report["backends"][-1]["passed"], "backend or cleanup did not pass")
    report["passed"] = all(row["passed"] for row in report["backends"])
    (args.output / "results.json").write_text(json.dumps(report, indent=2)+"\n", encoding="utf-8")
    require(report["passed"], "physical acceptance or cleanup failed; inspect results.json")
    print("Direct and SDK full-frame thermal/control acceptance passed.")


if __name__ == "__main__":
    main()
