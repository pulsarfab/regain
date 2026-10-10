"""Run external ConformU against eight explicitly simulated hub classes only.

Requires a built Regain server and stock ConformU by default; review builds
require an explicit label and a matching provenance manifest.
Never accepts an upstream URI, COM ProgID, existing hub config or hardware source.
Optional native ASCOM publication uses temporary per-user COM fixture aliases.
Logs/settings/results are retained in a fresh private artifacts directory.
"""
import argparse
from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parent.parent
CLASSES = (
    "switch", "safetymonitor", "observingconditions", "focuser", "rotator",
    "filterwheel", "covercalibrator", "camera",
)


def sha256(path):
    with path.open("rb") as content:
        return hashlib.file_digest(content, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--conformu", required=True, type=Path)
    parser.add_argument("--validator-kind", choices=("stock", "review"), default="stock",
                        help="Review requires the explicitly labelled ConformU review build; never replaces stock evidence")
    parser.add_argument("--bin-dir", default=ROOT / "target/debug", type=Path)
    parser.add_argument("--mode", choices=("protocol", "interface", "all"), default="all")
    parser.add_argument("--classes", nargs="+", choices=CLASSES, default=CLASSES)
    parser.add_argument("--camera-backend", choices=("simulated", "sdk-simulated", "direct-simulated"),
                        default="simulated", help="Explicit simulation only; never opens an SDK or USB device")
    parser.add_argument("--timeout-seconds", type=int, default=900)
    parser.add_argument("--native-ascom", choices=("x86", "x64"),
                        help="Check private native ASCOM exports; Windows, unelevated, interface mode only")
    args = parser.parse_args()
    if args.timeout_seconds <= 0:
        parser.error("--timeout-seconds must be positive")
    if args.native_ascom and (os.name != "nt" or args.mode != "interface"):
        parser.error("--native-ascom requires Windows and --mode interface")
    tool = args.conformu.resolve(strict=True)
    binary = args.bin_dir.resolve() / ("regain-alpaca.exe" if os.name == "nt" else "regain-alpaca")
    if not binary.is_file():
        parser.error(f"Build the Regain server first: {binary}")
    directory = ROOT / "artifacts" / f"hub-conformance-{uuid.uuid4().hex}"
    directory.mkdir(parents=True)
    config = json.loads((ROOT / "crates/regain-hub/examples/simulated-observatory.json").read_text(encoding="utf-8"))
    config.update(instanceId=str(uuid.uuid4()), revision=str(uuid.uuid4()))
    for output in config["outputs"]:
        output.update(number=40, label=f"PRIVATE ConformU {output['label']}")
    for kind in CLASSES[3:]:
        source = str(uuid.uuid4())
        config["sources"].append({"id": source, "label": f"PRIVATE simulated {kind}",
                                  "backend": {"kind": "simulated", "deviceType": kind}})
        config["outputs"].append({"id": str(uuid.uuid4()), "number": 40,
                                  "label": f"PRIVATE simulated {kind}",
                                  "device": {"kind": "proxy", "source": source, "deviceType": kind}})
    assert len(config["sources"]) == len(CLASSES)
    assert all(source["backend"]["kind"] == "simulated" for source in config["sources"])
    if args.camera_backend != "simulated":
        worker = binary.parent / ("regain-device.exe" if os.name == "nt" else "regain-device")
        if not worker.is_file():
            parser.error(f"Build the native simulation worker first: {worker}")
        direct = args.camera_backend == "direct-simulated"
        for source in config["sources"]:
            if source["backend"]["deviceType"] == "camera":
                source["backend"] = {"kind": "native", "device": "camera-direct" if direct else "camera-sdk",
                                     "identity": "direct-simulator" if direct else "sim00001",
                                     "camera": {"model": "ZWO ASI585MM Pro" if direct else "ZWO Simulated",
                                                "recovery": {"maxRetries": 0, "readyFrameDownloadRetries": 0,
                                                             "reconnectDelaySeconds": 0.01}}}
                source["polling"] = {"connectionTimeoutSeconds": 10.0, "requestTimeoutSeconds": 5.0,
                                     "pollSeconds": 60.0}
                break
    config_file = directory / "hub.json"
    config_file.write_text(json.dumps(config, indent=2), encoding="utf-8")
    profiles = directory / "profiles.json"
    profiles.write_text("[]", encoding="utf-8")
    settings = directory / "conformu-settings.json"
    # Every interface test remains enabled by ConformU's full-test command.
    # Only settle delays are shortened for the in-memory simulated Switch.
    settings.write_text(json.dumps({"SettingsCompatibilityVersion": 1,
                                   "RiskAcknowledged": True, "UpdateCheck": False,
                                   "SwitchReadDelay": 20, "SwitchWriteDelay": 50,
                                   "AlpacaConfiguration": {"ProtocolStrictChecks": True}}), encoding="utf-8")
    version = subprocess.run([str(tool), "--version"], capture_output=True, text=True,
                             check=True, timeout=30).stdout.strip()
    if ("regain-review" in version.lower()) != (args.validator_kind == "review"):
        parser.error("Validator label and tool version disagree")
    provenance = {"toolPath": str(tool), "toolSha256": sha256(tool),
                  "validatorKind": args.validator_kind,
                  "serverPath": str(binary), "serverSha256": sha256(binary)}
    if (tool.parent / "ConformU.dll").is_file():
        provenance["toolAssemblySha256"] = sha256(tool.parent / "ConformU.dll")
    if args.validator_kind == "review":
        manifest_path = tool.parent / "regain-review.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        expected = {"validatorKind": "review", "version": version,
                    "toolSha256": provenance["toolSha256"],
                    "toolAssemblySha256": provenance.get("toolAssemblySha256"),
                    "patchSha256": sha256(ROOT / "scripts/conformu-review/conformu-4.5.patch")}
        if any(manifest.get(key) != value for key, value in expected.items()):
            parser.error("Review manifest does not match this tool and patch")
        provenance["reviewManifestSha256"] = sha256(manifest_path)
        provenance["reviewPatchSha256"] = manifest["patchSha256"]
    if args.camera_backend != "simulated":
        provenance.update(workerPath=str(worker), workerSha256=sha256(worker))
    results = []
    modes = ("protocol", "interface") if args.mode == "all" else (args.mode,)
    print(f"ConformU: {version}\nPrivate simulation evidence: {directory}", flush=True)
    # Protocol tests may acquire images. Interface first-use checks need a
    # fresh host; client disconnect deliberately retains completed frames.
    for mode in modes:
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        base = f"http://127.0.0.1:{port}"
        with (directory / f"{mode}-server.log").open("w", encoding="utf-8") as server_log, \
                (directory / f"{mode}-host.log").open("w", encoding="utf-8") as host_log:
            flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
            host = subprocess.Popen([str(binary), "--hub-host", "--hub-config", str(config_file),
                                     "--workers", str(binary.parent), "--simulate"],
                                    stdout=host_log, stderr=host_log, creationflags=flags)
            server = None
            try:
                deadline = time.monotonic() + 30
                while "Regain hub ready:" not in (directory / f"{mode}-host.log").read_text(encoding="utf-8"):
                    if host.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError(f"Private host failed to bind; see {mode}-host.log")
                    time.sleep(0.05)
                # The owned host holds the lock before HTTP attachment, so the
                # frontend cannot launch an unowned persistent host during a race.
                server = subprocess.Popen([str(binary), "--listen", "127.0.0.1", "--port", str(port),
                                           "--hub-config", str(config_file), "--profiles", str(profiles),
                                           "--workers", str(binary.parent), "--simulate", "--no-discovery"],
                                          stdout=server_log, stderr=server_log, creationflags=flags)
                deadline = time.monotonic() + 30
                while True:
                    try:
                        with urllib.request.urlopen(base + "/management/v1/configureddevices", timeout=1) as response:
                            devices = json.load(response)
                        assert devices["ErrorNumber"] == 0 and len(devices["Value"]) == len(CLASSES), devices
                        assert all(d["DeviceNumber"] == 40 and "Simulation" in d["DeviceName"] for d in devices["Value"]), devices
                        break
                    except (urllib.error.URLError, TimeoutError):
                        if server.poll() is not None or time.monotonic() >= deadline:
                            raise RuntimeError(f"Private simulated hub failed to start; see {mode}-server.log")
                        time.sleep(0.05)
                # ConformU samples cover motion at 500 ms intervals. A 200 ms
                # simulator can finish before its Halt test even observes motion.
                # Set an observable travel duration using the ordinary, revision-
                # checked simulation controls; retain both request and response.
                controls = []
                for source in config["sources"]:
                    if source["backend"].get("deviceType") != "covercalibrator":
                        continue
                    command = {"op": "updateSimulation", "source": source["id"],
                               "expectedRevision": config["revision"],
                               "update": {"coverCalibrator": {"moveDurationSeconds": 2.0}}}
                    request = urllib.request.Request(base + "/setup/api/hub",
                                                     data=json.dumps(command).encode("utf-8"),
                                                     headers={"Content-Type": "application/json", "Origin": base})
                    with urllib.request.urlopen(request, timeout=5) as response:
                        applied = json.load(response)
                    if "result" not in applied:
                        raise RuntimeError(f"Private simulation control failed: {applied}")
                    controls.append({"request": command, "response": applied})
                (directory / f"{mode}-simulation-controls.json").write_text(json.dumps(controls, indent=2), encoding="utf-8")
                with ExitStack() as publication:
                    native = None
                    if args.native_ascom:
                        from hub_conformance_com import publish
                        native = publication.enter_context(publish(binary, config, directory, args.native_ascom))
                        provenance.update(nativeAscom=native, nativeAscomSha256=sha256(Path(native["serverPath"])))
                    run_checks(args, tool, base, settings, directory, mode, results, native)
            finally:
                for process in (server, host):
                    if process is not None:
                        if process.poll() is None:
                            process.terminate()
                        try:
                            process.wait(timeout=15)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait(timeout=5)
                (directory / "summary.json").write_text(json.dumps({"toolVersion": version, "simulationOnly": True,
                                                                    "cameraBackend": args.camera_backend,
                                                                    "publication": "native-ascom" if args.native_ascom else "alpaca",
                                                                    "nativeAscomArchitecture": args.native_ascom,
                                                                    "config": str(config_file), "provenance": provenance,
                                                                    "results": results}, indent=2), encoding="utf-8")
    return 0 if results and all(result["passed"] for result in results) else 1


def run_checks(args, tool, base, settings, directory, mode, results, native):
    for kind in args.classes:
        stem = f"{kind}-{mode}"
        log = directory / f"{stem}.log"
        console = directory / f"{stem}-console.log"
        report = directory / f"{stem}.json"
        target = native["devices"][kind] if native else f"{base}/api/v1/{kind}/40"
        command = [str(tool), "alpacaprotocol" if mode == "protocol" else "conformance",
                   target, "-s", str(settings), "-n", str(log), "-r", str(report)]
        record = {"class": kind, "mode": mode, "command": command, "log": str(log)}
        results.append(record)
        with console.open("w", encoding="utf-8") as output:
            try:
                completed = subprocess.run(command, stdout=output, stderr=subprocess.STDOUT,
                                           timeout=args.timeout_seconds)
                record["exitCode"] = completed.returncode
            except subprocess.TimeoutExpired:
                record.update(exitCode=None, timedOut=True)
        text = console.read_text(encoding="utf-8")
        if mode == "protocol":
            # ConformU 4.5's protocol command does not write --resultsfile.
            counts = re.search(r"Found (\d+) errors?, (\d+) issues? and (\d+) information messages?", text)
            if counts:
                record.update(errors=int(counts[1]), issues=int(counts[2]), information=int(counts[3]))
            elif "Congratulations there were no errors, issues or information alerts - Your device passes ASCOM Alpaca protocol validation!!" in text:
                record.update(errors=0, issues=0, information=0)
            else:
                record["missingSummary"] = True
        elif report.is_file():
            detail = json.loads(report.read_text(encoding="utf-8-sig"))
            record.update(errors=detail["ErrorCount"], issues=detail["IssueCount"],
                          configurationAlerts=detail["ConfigurationAlertCount"],
                          timingIssues=detail["TimingIssuesCount"], report=str(report))
        else:
            record["missingSummary"] = True
        record["passed"] = (record["exitCode"] == 0 and not record.get("missingSummary")
                            and record.get("errors") == 0 and record.get("issues") == 0
                            and record.get("configurationAlerts", 0) == 0
                            and record.get("timingIssues", 0) == 0)
        print(f"{stem}: {'PASS' if record['passed'] else 'FAIL'} {json.dumps(record)}", flush=True)


if __name__ == "__main__":
    raise SystemExit(main())
