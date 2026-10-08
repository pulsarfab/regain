"""Measure cached panel COM reads through an unmodified ConformU facade.

Private simulation only: no existing configuration, COM identity or equipment.
Cold facade reads precede split-phase diagnostics; neither replaces conformance.
Requires Windows, .NET 10 SDK and an unmodified built ConformU 4.5.0.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]


def main():
    if sys.platform != "win32" or not __debug__:
        raise RuntimeError("Timing diagnostics require Windows and enabled Python assertions")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--conformu", type=Path, required=True)
    args = parser.parse_args()
    from hub_conformance_com import publish
    folder = ROOT / "artifacts" / ("hub-state-timing-" + uuid.uuid4().hex)
    folder.mkdir()
    binary = ROOT / "target/debug/regain-alpaca.exe"
    project = ROOT / "scripts/fixtures/hub-state-timing/StateTiming.csproj"
    tool = args.conformu.resolve(strict=True)
    flags = subprocess.CREATE_NO_WINDOW
    print(folder, flush=True)
    with (folder / "build.log").open("w", encoding="utf-8") as build_log:
        subprocess.run(["dotnet", "build", str(project), "-c", "Release",
                        "-p:ConformuDir=" + str(tool.parent), "-o", str(folder / "client"),
                        "-p:BaseIntermediateOutputPath=" + str(folder / "obj") + "/"],
                       stdout=build_log, stderr=subprocess.STDOUT, check=True, timeout=120, creationflags=flags)
    version = subprocess.run([str(tool), "--version"], capture_output=True, text=True, check=True,
                             timeout=30, creationflags=flags).stdout.strip()
    client = folder / "client/StateTiming.exe"
    run_probe(folder, binary, client, publish, version, tool)


def run_probe(folder, binary, client, publish, version, tool):
    source = str(uuid.uuid4())
    config = dict(schemaVersion=1, revision=str(uuid.uuid4()), instanceId=str(uuid.uuid4()),
              sources=[dict(id=source, label="PRIVATE panel timing simulation", backend=dict(kind="simulated", deviceType="covercalibrator"))],
              outputs=[dict(id=str(uuid.uuid4()), number=0, label="PRIVATE panel timing simulation",
                            device=dict(kind="proxy", source=source, deviceType="covercalibrator"))])
    config_file = folder / "hub.json"
    config_file.write_text(json.dumps(config), encoding="utf-8")
    flags = subprocess.CREATE_NO_WINDOW
    results = dict(simulationOnly=True, diagnosticOnly=True, toolVersion=version,
                   toolSha256=hashlib.sha256(tool.read_bytes()).hexdigest(),
                   facadeSha256=hashlib.sha256((tool.parent / "ConformU.dll").read_bytes()).hexdigest(),
                   serverSha256=hashlib.sha256(binary.read_bytes()).hexdigest(), clients=[])
    with (folder / "host.log").open("w", encoding="utf-8") as log:
        host = subprocess.Popen([str(binary), "--hub-host", "--hub-config", str(config_file), "--simulate"],
                                stdout=log, stderr=log, creationflags=flags)
        try:
            deadline = time.monotonic() + 30
            while "Regain hub ready:" not in (folder / "host.log").read_text(encoding="utf-8"):
                if host.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError("Owned simulated host failed to start")
                time.sleep(0.05)
            with publish(binary, config, folder, "x64") as native:
                results["native"] = native
                results["nativeSha256"] = hashlib.sha256(Path(native["serverPath"]).read_bytes()).hexdigest()
                for index in range(3):
                    record = dict(index=index)
                    results["clients"].append(record)
                    with (folder / f"client-{index}.out").open("w", encoding="utf-8") as output, \
                            (folder / f"client-{index}.err").open("w", encoding="utf-8") as errors:
                        try:
                            run = subprocess.run([str(client), native["devices"]["covercalibrator"], str(folder)],
                                                 stdout=output, stderr=errors, timeout=45, creationflags=flags)
                            record["exitCode"] = run.returncode
                            run.check_returncode()
                        except subprocess.TimeoutExpired:
                            record["timedOut"] = True
                            raise
                    record["timing"] = json.loads((folder / f"client-{index}.out").read_text(encoding="utf-8"))
        finally:
            if host.poll() is None:
                host.terminate()
            try:
                host.wait(timeout=10)
            except subprocess.TimeoutExpired:
                host.kill()
                host.wait(timeout=5)
            results["hostStopped"] = host.poll() is not None
            (folder / "summary.json").write_text(json.dumps(results, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
