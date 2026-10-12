"""Opt-in full-frame ASI6200MM Pro abort/reuse acceptance with passive USB tracing.

Run with the existing inspection Python environment (Frida is required). Only
the worker started here is controlled. The camera must have no other owner.
Every acquisition, including aborted frames, uses 9576x6388 RAW16 at bin 1.
Offset baselines and post-abort verification exposures are always 5 seconds.
No USB reset, SDK loading, process replacement, or external process control is
performed. Results contain hashes/statistics, never image buffers. USB traces
contain the selected device's private path and must not be published verbatim.
"""
import argparse
import array
import hashlib
import json
import math
import queue
import statistics
import struct
import subprocess
import sys
import threading
import time
import traceback
from pathlib import Path


MODEL = "ZWO ASI6200MM Pro"
WIDTH, HEIGHT = 9576, 6388
CASES = {
    "immediate": (15, None, 0),
    "delay-170ms": (15, None, .17),
    "initializing": (15, "initializing", 0),
    "exposing": (15, "exposing", 0),
    "synchronizing": (15, "synchronizing", 0),
    "integrating": (15, "integrating", 0),
    "integrating-620ms": (15, "integrating", .62),
    "integrating-820ms": (15, "integrating", .82),
    "integrating-1020ms": (15, "integrating", 1.02),
    "restoring-clocks": (5, "restoring_clocks", 0),
    "downloading": (5, "downloading", 0),
}
# SDK-style queued normal reads have no separate sensor_readout phase. The
# downloading case cancels active batch harvesting after clock restoration.
DEFAULT_CASES = list(CASES)
# These phases also exist in older release workers, for before/after comparisons.
for _delay in [.17, .62, .82, 1.02]:
    CASES[f"exposing-{round(_delay * 1000)}ms"] = (15, "exposing", _delay)


def require(condition, message):
    if not condition:
        raise AssertionError(message)


class Worker:
    """The framing from test-rust.py, with deadline-bound readers and phases."""

    def __init__(self, executable, output, trace_source):
        import frida
        self.output = output
        self.sequence = 0
        self.events = []
        self.trace_events = []
        self.condition = threading.Condition()
        self.replies = queue.Queue()
        self.trace_lock = threading.Lock()
        self.log = (output / "worker.log").open("x", encoding="utf-8")
        self.trace = (output / "private-usb.jsonl").open("x", encoding="utf-8")
        self.process = subprocess.Popen(
            [str(executable), "zwo", "camera-direct", "--serve"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.pid = self.process.pid
        self.stdout_thread = threading.Thread(target=self._stdout, daemon=True)
        self.stderr_thread = threading.Thread(target=self._stderr, daemon=True)
        self.stdout_thread.start()
        self.stderr_thread.start()
        self.session = None
        self.script = None
        try:
            self.session = frida.get_local_device().attach(self.pid)
            self.script = self.session.create_script(trace_source.read_text(encoding="utf-8"))
            self.script.on("message", self._trace)
            self.script.load()
            self.record_trace(dict(kind="acceptance-worker", pid=self.pid,
                                   sha256=hashlib.sha256(executable.read_bytes()).hexdigest()))
        except BaseException:
            self.finish()
            raise

    def record_trace(self, event):
        with self.trace_lock:
            event = dict(event, observedMonotonic=time.monotonic())
            self.trace_events.append(event)
            self.trace.write(json.dumps(event) + "\n")
            self.trace.flush()

    def _trace(self, message, data):
        # All injection globals in trace-transport.js are unset: passive only.
        payload = message.get("payload", message)
        if data is not None:
            payload = dict(payload, payloadBytes=len(data),
                           payloadSha256=hashlib.sha256(data).hexdigest())
        self.record_trace(payload)

    def _read(self, length):
        data = bytearray()
        while len(data) < length:
            chunk = self.process.stdout.read(length - len(data))
            if not chunk:
                raise EOFError("owned worker ended before completing reply")
            data.extend(chunk)
        return bytes(data)

    def _stdout(self):
        try:
            while True:
                length, = struct.unpack("<I", self._read(4))
                require(0 < length <= 65536, "invalid worker JSON reply size")
                reply = json.loads(self._read(length))
                binary_length = reply["binaryLength"]
                require(0 <= binary_length <= 512 * 1024 * 1024,
                        "invalid worker image reply size")
                self.replies.put((reply, self._read(binary_length)))
        except BaseException as error:
            self.replies.put(error)

    def _stderr(self):
        for raw in iter(self.process.stderr.readline, b""):
            line = raw.decode(errors="replace")
            self.log.write(line)
            self.log.flush()
            if line.startswith("REGAIN_DIAGNOSTIC "):
                try:
                    event = json.loads(line[len("REGAIN_DIAGNOSTIC "):])
                    event["observedMonotonic"] = time.monotonic()
                    with self.condition:
                        self.events.append(event)
                        self.condition.notify_all()
                except (ValueError, TypeError):
                    pass

    def call(self, method, params=None, timeout=35):
        require(self.process.poll() is None, "owned worker exited")
        self.sequence += 1
        request = json.dumps(dict(version=1, id=self.sequence, method=method,
                                  params=params or {})).encode()
        self.process.stdin.write(struct.pack("<I", len(request)) + request)
        self.process.stdin.flush()
        try:
            result = self.replies.get(timeout=timeout)
        except queue.Empty as error:
            raise TimeoutError(f"{method} reply exceeded {timeout}s") from error
        if isinstance(result, BaseException):
            raise result
        reply, pixels = result
        require(reply["version"] == 1 and reply["id"] == self.sequence,
                f"worker reply sequence mismatch: {reply}")
        require(reply["ok"], f"{method} failed: {reply}")
        return reply.get("result"), pixels

    def phase(self, name, since, timeout=25):
        deadline = time.monotonic() + timeout
        with self.condition:
            while True:
                matches = [e for e in self.events[since:]
                           if e.get("event") == "capture.phase"
                           and e.get("details", {}).get("phase") == name]
                if matches:
                    return matches[0]
                remaining = deadline - time.monotonic()
                require(remaining > 0, f"capture never reached phase {name}")
                self.condition.wait(min(remaining, .25))
                require(self.process.poll() is None, "worker exited while waiting for phase")

    def get(self, control):
        return self.call("get", dict(control=control))[0]

    def set(self, control, value):
        return self.call("set", dict(control=control, value=value))[0]

    def physical_thermal(self, since, timeout=1):
        # Frida delivers messages asynchronously. No new camera command is
        # needed: abort verification and disconnect already read BC19.
        deadline = time.monotonic() + timeout
        previous_count = -1
        changed = time.monotonic()
        while True:
            with self.trace_lock:
                observed = thermal_readback(self.trace_events[since:])
                count = len(self.trace_events)
            if count != previous_count:
                previous_count = count
                changed = time.monotonic()
            if observed is not None and time.monotonic() - changed >= .05:
                return observed
            if time.monotonic() >= deadline:
                require(observed is not None, "no physical BC19 thermal readback was traced")
                return observed
            time.sleep(.01)

    def frame(self, exposure):
        before = len(self.trace_events)
        self.call("start", exposure)
        deadline = time.monotonic() + exposure["microseconds"] / 1e6 + 35
        while True:
            state = self.call("status")[0]
            if state == 2:
                break
            require(state == 1, f"unexpected capture state {state}")
            require(time.monotonic() < deadline, "fresh capture deadline expired")
            time.sleep(.02)
        metadata, pixels = self.call("download", timeout=45)
        require(len(pixels) == exposure["width"] * exposure["height"] * 2,
                "frame byte count does not match geometry")
        require(metadata.get("model") == "ASI6200MM Pro", "incorrect capture model")
        require(metadata.get("sdkLoaded") is False, "capture loaded vendor SDK")
        require(metadata.get("readRecoveries") == 0, "fresh capture needed a read recovery")
        require("cleanupError" not in metadata, "fresh capture cleanup failed")
        for key in ("width", "height", "microseconds"):
            require(metadata.get(key) == exposure[key], f"incorrect capture {key}")
        digest = hashlib.sha256(pixels).hexdigest()
        require(metadata.get("sha256") == digest, "frame hash differs from worker metadata")
        starts = sum(e.get("kind") == "io-submit" and
                     (e.get("header") or "").startswith("40a9")
                     for e in self.trace_events[before:])
        require(starts == 1, f"fresh capture issued {starts} physical exposure starts")
        return dict(metadata=metadata, sha256=digest,
                    statistics=pixel_statistics(pixels, exposure["width"], exposure["height"]),
                    exposureStarts=starts)

    def finish(self):
        if self.process.poll() is None:
            try:
                self.process.stdin.close()
                self.process.wait(timeout=4)
            except (OSError, subprocess.TimeoutExpired):
                self.process.kill()  # Only this harness's child, never another process.
                self.process.wait(timeout=4)
        if self.script is not None:
            try:
                self.script.unload()
            except Exception:
                pass
        if self.session is not None:
            try:
                self.session.detach()
            except Exception:
                pass
        self.stdout_thread.join(timeout=2)
        self.stderr_thread.join(timeout=2)
        self.log.close()
        self.trace.close()


def pixel_statistics(pixels, width, height):
    """Exact median of every row; sampled bands for compact context.

    NumPy accelerates the 61 megapixel full-frame check when available in the
    inspection environment. The dependency-free fallback checks the same rows.
    """
    try:
        import numpy as np
    except ImportError:
        np = None
    if np is not None:
        values = np.frombuffer(pixels, dtype="<u2")
        row_medians = np.median(values.reshape(height, width), axis=1)
        row_range = [float(np.min(row_medians)), float(np.max(row_medians))]
        algorithm = "numpy-exact-every-row"
    else:
        values = array.array("H", pixels)
        if sys.byteorder != "little":
            values.byteswap()
        row_medians = [statistics.median(values[row * width:(row + 1) * width])
                       for row in range(height)]
        row_range = [min(row_medians), max(row_medians)]
        algorithm = "python-exact-every-row"
    bands = []
    samples = []
    for band in range(16):
        start, end = height * band // 16 * width, height * (band + 1) // 16 * width
        sample = values[start:end:max(1, (end - start) // 1024)]
        if np is not None:
            sample = sample.tolist()
        samples.extend(sample)
        bands.append(dict(mean=statistics.fmean(sample), median=statistics.median(sample),
                          saturation=sum(v >= 65500 for v in sample) / len(sample)))
    return dict(mean=statistics.fmean(samples), median=statistics.median(samples),
                standardDeviation=statistics.pstdev(samples), samples=len(samples),
                saturation=sum(v >= 65500 for v in samples) / len(samples), bands=bands,
                rowMedianRange=row_range, rowMedianRows=height, rowMedianAlgorithm=algorithm)


def exposure(seconds):
    return dict(width=WIDTH, height=HEIGHT,
                x=0, y=0, bin=1, microseconds=round(seconds * 1e6), dark=True, readRetries=0)


def thermal_readback(events):
    """Latest completed BC19 read, plus any later BD19 writes (passive only)."""
    latest = None
    for event in events:
        if event.get("kind") != "io-complete" or not event.get("ok"):
            continue
        header = bytes.fromhex(event.get("header") or "")
        if len(header) < 8 or int.from_bytes(header[2:4], "little") != 0x19:
            continue
        if header[:2] == b"\xc0\xbc" and event.get("controlReply"):
            value = int.from_bytes(bytes.fromhex(event["controlReply"]), "little")
            latest = dict(register="BC19", value=value, thermalBits=value & 0xc0,
                          sequence=event["sequence"], timeMs=event.get("timeMs"),
                          observedMonotonic=event.get("observedMonotonic"), subsequentWrites=[])
        elif header[:2] == b"\x40\xbd" and latest is not None:
            latest["subsequentWrites"].append(dict(sequence=event["sequence"],
                value=int.from_bytes(header[4:6], "little")))
    return latest


def verify_thermal_readback(observed, expected, mask):
    require(observed["value"] & mask == expected & mask,
            f"physical BC19 thermal flags {observed['value']:#04x} do not match expected {expected:#04x} (mask {mask:#04x})")
    # Close may perform repeated off writes. Only idempotent thermal writes
    # following an observed off state preserve the physical readback proof.
    require(all(w["value"] & mask == observed["value"] & mask
                for w in observed["subsequentWrites"]),
            "physical BC19 readback preceded a different thermal-state write")


def freshness(frame, reference, other):
    """Use an observed relative offset response, never an absolute ADU target."""
    require(frame["sha256"] != other["sha256"], "follow-up returned opposite-offset baseline pixels")
    current, expected, opposite = (f["statistics"] for f in (frame, reference, other))
    checks = []
    response = abs(expected["median"] - opposite["median"])
    if max(current["saturation"], expected["saturation"], opposite["saturation"]) > .01:
        checks.append(dict(scope="every-row", checked=False, reason="saturated"))
    elif response < 10 or (expected["rowMedianRange"][1] - expected["rowMedianRange"][0]) >= response / 2:
        checks.append(dict(scope="every-row", checked=False, reason="row offset response inconclusive"))
    else:
        lower = expected["rowMedianRange"][0] - response / 2
        upper = expected["rowMedianRange"][1] + response / 2
        passed = lower < current["rowMedianRange"][0] and current["rowMedianRange"][1] < upper
        checks.append(dict(scope="every-row", checked=True, passed=passed,
                           rows=current["rowMedianRows"], rowMedianRange=current["rowMedianRange"],
                           expectedInterval=[lower, upper], offsetResponse=response))
    for band, (actual, wanted, old) in enumerate(zip(current["bands"], expected["bands"], opposite["bands"])):
        response = wanted["median"] - old["median"]
        if max(actual["saturation"], wanted["saturation"], old["saturation"]) > .01:
            checks.append(dict(band=band, checked=False, reason="saturated"))
        elif abs(response) < 10:
            checks.append(dict(band=band, checked=False, reason="offset response inconclusive"))
        else:
            # Allow drift up to half the measured offset response. This detects
            # rows retained from the opposite-offset frame, without assuming a cap.
            passed = abs(actual["median"] - wanted["median"]) < abs(response) / 2
            checks.append(dict(band=band, checked=True, passed=passed,
                               median=actual["median"], expectedMedian=wanted["median"],
                               oppositeMedian=old["median"]))
    require(all(c.get("passed", True) for c in checks),
            "follow-up row-band offset response changed or retained opposite-offset rows")
    return checks


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--hardware", action="store_true", help="explicitly authorize opening the selected real camera")
    parser.add_argument("--worker", type=Path, required=True, help="exact regain-device.exe to test")
    parser.add_argument("--serial", required=True, help="exact 16-digit serial from read-only enumeration")
    parser.add_argument("--output", type=Path, required=True, help="new directory for private trace and results")
    parser.add_argument("--repeat", type=int, default=1, help="repeat full-frame cancellation cases (1..10)")
    parser.add_argument("--case", choices=list(CASES), action="append",
                        help="select full-frame cases; default is the entire current-phase matrix (exposing-delay aliases support older workers)")
    parser.add_argument("--thermal", action="store_true", help="verify enabled cooler/heater survive cancellation, including physical BC19 readbacks")
    parser.add_argument("--skip-freshness-baseline", action="store_true",
                        help="omit paired 5-second full-frame offset baselines for a targeted old/fixed comparison; verification frames remain full-frame")
    args = parser.parse_args()
    if not args.hardware:
        parser.error("physical acceptance requires explicit --hardware")
    if not 1 <= args.repeat <= 10:
        parser.error("--repeat must be 1..10")
    if len(args.serial) != 16 or any(c not in "0123456789abcdefABCDEF" for c in args.serial) or int(args.serial, 16) == 0:
        parser.error("--serial must be an exact nonzero 16-digit camera serial")
    args.worker = args.worker.resolve(strict=True)
    args.output.mkdir(parents=True, exist_ok=False)
    results = dict(worker=str(args.worker), workerSha256=hashlib.sha256(args.worker.read_bytes()).hexdigest(),
                   scriptSha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                   serial=args.serial.lower(), cases=[], passed=False, cleanup=[])
    worker = None
    opened = False
    controls = {}
    thermal_mask = 0xc0
    try:
        worker = Worker(args.worker, args.output, Path(__file__).with_name("trace-transport.js"))
        results["pid"] = worker.pid
        listed, _ = worker.call("list", dict(serials=True))
        selected = [c for c in listed if c.get("name") == MODEL and
                    str(c.get("serial", "")).lower() == args.serial.lower()]
        require(len(selected) == 1, "exact ASI6200MM Pro serial is not uniquely available")
        results["listedIdentity"] = selected[0]
        identity, _ = worker.call("open", dict(name=MODEL, serial=args.serial.lower()))
        opened = True
        require(str(identity.get("serial", "")).lower() == args.serial.lower(), "opened serial mismatch")
        require(identity.get("info", {}).get("name") == MODEL, "opened model mismatch")
        results["identity"] = identity
        controls = {c["type"]: c for c in identity["controls"]}
        thermal_mask = 0x80 | (0x40 if 21 in controls else 0)
        results["initialControls"] = {str(c): worker.get(c) for c in controls}
        worker.set(0, 100)
        if args.thermal:
            ambient = worker.get(8) / 10
            target = min(20, max(-5, math.floor(ambient - 2)))
            worker.set(16, target)
            worker.set(17, 1)
            if 21 in controls:
                worker.set(21, 1)
            results["thermalTestTarget"] = target
        thermal = {str(c): worker.get(c) for c in [16, 17, 21] if c in controls}
        results["expectedThermalControls"] = thermal
        thermal_flags = (0 if thermal.get("17") else 0x80) | (0x40 if thermal.get("21") else 0)
        worker.set(5, 50)
        results["fullFrameBaseline"] = worker.frame(exposure(5))
        baselines = {}
        if not args.skip_freshness_baseline:
            baselines[50] = results["fullFrameBaseline"]
            worker.set(5, 100)
            baselines[100] = worker.frame(exposure(5))
            results["fullFrameBaselines"] = baselines
            require(baselines[50]["sha256"] != baselines[100]["sha256"], "baseline offset change returned identical frames")
        else:
            results["fullFrameBaselinesSkipped"] = True
        # Device opens used by list/identity are allowed before acceptance starts.
        trace_start = len(worker.trace_events)
        previous_hash = (baselines[100] if baselines else results["fullFrameBaseline"])["sha256"]
        for repetition in range(args.repeat):
            for name in args.case or DEFAULT_CASES:
                seconds, phase, delay = CASES[name]
                case = dict(name=name, repetition=repetition + 1, passed=False)
                results["cases"].append(case)
                offset = 50 if len(results["cases"]) % 2 else 100
                worker.set(5, 150 - offset)  # Aborted frame has the opposite offset.
                marker = len(worker.events)
                started = time.monotonic()
                case["startMonotonic"] = started
                case["abortTraceStart"] = len(worker.trace_events)
                worker.call("start", exposure(seconds))
                if phase:
                    observed = worker.phase(phase, marker)
                    case["targetPhase"] = observed
                    delay_until = observed["observedMonotonic"] + delay
                else:
                    delay_until = started + delay
                if delay_until > time.monotonic():
                    time.sleep(delay_until - time.monotonic())
                case["stopRequestedMonotonic"] = time.monotonic()
                case["stopRequestedAfterSeconds"] = case["stopRequestedMonotonic"] - started
                case["phasesAtStop"] = worker.events[marker:]
                stop_start = time.monotonic()
                worker.call("stop", timeout=25)
                case["stopAcknowledgedMonotonic"] = time.monotonic()
                case["stopAcknowledgedAfterSeconds"] = case["stopAcknowledgedMonotonic"] - stop_start
                case["abortTraceEnd"] = len(worker.trace_events)
                starts = sum(e.get("kind") == "io-submit" and
                             (e.get("header") or "").startswith("40a9")
                             for e in worker.trace_events[case["abortTraceStart"]:case["abortTraceEnd"]])
                case["abortExposureStarts"] = starts
                require(starts <= 1, f"abort issued {starts} exposure starts")
                # stderr and stdout have independent readers. Allow a short
                # delivery grace for the terminal diagnostic emitted before ack.
                grace = time.monotonic() + 1
                with worker.condition:
                    while True:
                        phases = [e for e in worker.events[marker:]
                                  if e.get("event") == "capture.phase"]
                        terminal = phases[-1].get("details", {}).get("phase") if phases else None
                        if terminal in ["failed", "image_ready"] or time.monotonic() >= grace:
                            break
                        worker.condition.wait(.01)
                case["abortPhases"] = phases
                case["terminalPhaseAtStop"] = terminal
                case["activeCancellationExercised"] = terminal == "failed"
                require(terminal == "failed",
                        f"{name} stage unexercised: stop acknowledged terminal phase {terminal}; active cancellation was not observed")
                require(worker.call("status")[0] == 0, "abort did not leave worker idle")
                require(worker.process.pid == worker.pid and worker.process.poll() is None, "worker was replaced")
                actual = {str(c): worker.get(c) for c in [16, 17, 21] if c in controls}
                case["thermalControlsAfterAbort"] = actual
                require(actual == thermal, "abort changed thermal control settings")
                if args.thermal:
                    observed = worker.physical_thermal(case["abortTraceStart"])
                    case["physicalThermalAfterAbort"] = dict(observed, expected=thermal_flags, mask=thermal_mask)
                    verify_thermal_readback(observed, thermal_flags, thermal_mask)
                worker.set(5, offset)
                # The reported failure is in long-mode synchronization. A
                # short-mode follow-up alone would not exercise that handshake.
                frame = worker.frame(exposure(5))
                require(frame["metadata"]["offset"] == offset, "follow-up offset metadata mismatch")
                require(frame["sha256"] != previous_hash, "follow-up reused previous frame pixels")
                case["freshFrame"] = frame
                if baselines:
                    case["rowBandFreshness"] = freshness(frame, baselines[offset], baselines[150 - offset])
                else:
                    case["rowBandFreshness"] = dict(checked=False, reason="paired baseline explicitly skipped")
                previous_hash = frame["sha256"]
                case["phases"] = worker.events[marker:]
                disallowed = [e for e in worker.events[marker:] if e.get("event") == "capture.phase" and
                              e.get("details", {}).get("phase") in ["reopening_retained", "restarting_retained"]]
                require(not disallowed, "acceptance required reopening/recovery")
                require(not any(e.get("kind") in ["device-open", "device-close", "sdk-module"]
                                for e in worker.trace_events[trace_start:]), "device was reopened/closed or SDK was loaded")
                case["passed"] = True
                (args.output / "results.json").write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
                print(f"PASS {name} repeat {repetition + 1}: stop {case['stopAcknowledgedAfterSeconds']:.3f}s; fresh full frame, PID {worker.pid}", flush=True)
        results["passed"] = True
    except BaseException as error:
        results["error"] = f"{type(error).__name__}: {error}"
        results["traceback"] = traceback.format_exc()
        print(results["error"], file=sys.stderr, flush=True)
    finally:
        if worker is not None:
            if opened:
                def cleanup(label, action):
                    try:
                        value = action()
                        results["cleanup"].append(dict(action=label, ok=True, value=value))
                    except BaseException as error:
                        results["cleanup"].append(dict(action=label, ok=False, error=str(error)))
                cleanup("stop", lambda: worker.call("stop", timeout=25)[0])
                # Restore ordinary settings, then explicitly switch actuators off:
                # disconnect never leaves a previously enabled cooler/heater on.
                initial = results.get("initialControls", {})
                for control in [0, 5, 16]:
                    if str(control) in initial and controls[control].get("writable"):
                        cleanup(f"restore {control}", lambda c=control: worker.set(c, initial[str(c)]))
                shutdown_trace_start = len(worker.trace_events)
                for control in [17, 21]:
                    if control in controls:
                        def disable(c=control):
                            worker.set(c, 0)
                            value = worker.get(c)
                            require(value == 0, f"control {c} did not disable")
                            return value
                        cleanup(f"disable {control}", disable)
                cleanup("close", lambda: worker.call("close", timeout=25)[0])
                if args.thermal:
                    def verify_shutdown():
                        observed = worker.physical_thermal(shutdown_trace_start)
                        results["physicalThermalAfterDisconnect"] = dict(observed, expected=0x80, mask=thermal_mask)
                        verify_thermal_readback(observed, 0x80, thermal_mask)
                        return observed
                    cleanup("physical thermal shutdown", verify_shutdown)
            results["diagnostics"] = worker.events
            worker.finish()
            results["workerExitCode"] = worker.process.returncode
        if any(not c["ok"] for c in results["cleanup"]):
            results["passed"] = False
        (args.output / "results.json").write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
    return 0 if results["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
