"""Hardware cooler/auxiliary recovery test through an isolated Rust Alpaca server.

Disconnect other camera apps first. This enables cooling and dew heating,
changes fan/LED settings, and terminates one owned worker per backend.
Restores the starting controls and disconnects in finally. Requires Frida on
Windows to verify actual USB register traffic, not just cached API values.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import signal
import socket
import struct
import subprocess
import threading
import time
import urllib.parse
import urllib.request

import frida

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--camera-name', required=True)
    parser.add_argument('--output', type=Path, required=True, help='new results directory')
    parser.add_argument('--worker-directory', type=Path, default=ROOT / 'target/release')
    parser.add_argument('--sdk', type=Path, default=ROOT / 'vendor/zwo/ASICamera2.dll')
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    base = f'http://127.0.0.1:{port}'

    def request(path, data=None, binary=False):
        setup = path.startswith('/setup/')
        headers = {'Accept': 'application/imagebytes'} if binary else {}
        if data is not None:
            headers['Content-Type'] = 'application/json' if setup else 'application/x-www-form-urlencoded'
            data = (json.dumps(data) if setup else urllib.parse.urlencode(data)).encode()
        method = 'GET' if data is None else 'POST' if setup else 'PUT'
        with urllib.request.urlopen(urllib.request.Request(base + path, data, headers, method=method), timeout=90) as response:
            value = response.read()
        return value if binary else json.loads(value)

    def camera(member, data=None, binary=False):
        value = request('/api/v1/camera/0/' + member + '?ClientID=911',
                        dict(data, ClientID=911) if data is not None else None, binary)
        if binary:
            return value
        if value['ErrorNumber']:
            raise RuntimeError(value)
        return value.get('Value')

    def diagnostics():
        return json.loads(camera('action', {'Action': 'ZwoGain.Diagnostics', 'Parameters': ''}))

    def control(kind, value):
        camera('action', {'Action': 'ZwoGain.SetControl',
                          'Parameters': json.dumps({'control': kind, 'value': value})})

    def wait_for(predicate, seconds, description):
        deadline = time.monotonic() + seconds
        while True:
            value = predicate()
            if value:
                return value
            if time.monotonic() >= deadline:
                raise RuntimeError(description)
            time.sleep(.2)

    results = {'camera': args.camera_name, 'builds': {}, 'modes': []}
    for name in ('zwogain-alpaca.exe', 'zwogain-direct.exe', 'zwogain-host.exe'):
        results['builds'][name] = hashlib.sha256((args.worker_directory / name).read_bytes()).hexdigest()
    records = []
    lock = threading.Lock()
    trace_file = (args.output / 'environment-usb.jsonl').open('w')
    server_log = (args.output / 'server.log').open('wb')
    process = subprocess.Popen([str((args.worker_directory / 'zwogain-alpaca.exe').resolve()),
                                '--port', str(port), '--profiles', str((args.output / 'profiles.json').resolve()),
                                '--sdk', str(args.sdk.resolve()), '--no-discovery'],
                               stdout=server_log, stderr=server_log, creationflags=subprocess.CREATE_NO_WINDOW)
    traces = []
    mode = ''

    def attach(pid):
        session = frida.attach(pid)
        script = session.create_script(Path(__file__).with_name('trace-environment.js').read_text())
        attached_mode = mode

        def message(message, data):
            row = dict(message.get('payload', message), mode=attached_mode)
            with lock:
                records.append(row)
                trace_file.write(json.dumps(row) + '\n')
                trace_file.flush()

        script.on('message', message)
        script.load()
        traces.append((session, script))

    def wire_check(kind, value, start):
        register = {21: 0x19, 22: 0xfa, 23: 0xfb}[kind]
        with lock:
            rows = list(records[start:])
        good = [r for r in rows if r.get('kind') == 'environment-usb'
                and r['ntStatus'] == 0 and r['usbStatus'] == 0 and r['register'] == register]
        writes = [r for r in good if r['request'] == 0xbd
                  and (bool(r['value'] & 0x40) == bool(value) if kind == 21 else r['value'] == value)]
        reads = [r for r in good if r['request'] == 0xbc and r['reply']
                 and (bool(r['reply'][0] & 0x40) == bool(value) if kind == 21 else r['reply'][0] == value)]
        # Dew sets a flag after reading it; the next toggle proves that flag
        # reached hardware. Fan and LED setters/readbacks return the new value.
        return bool(writes) and (kind == 21 or bool(reads))

    try:
        for _ in range(100):
            try:
                request('/setup/api/state')
                break
            except OSError:
                time.sleep(.1)
        for direct, fallback in ((False, False), (True, False), (True, True)):
            mode = 'direct-fallback' if fallback else 'direct' if direct else 'sdk'
            descriptors = request('/setup/api/discover', {'direct': direct})
            matches = [d for d in descriptors if d['name'] == args.camera_name]
            assert len(matches) == 1, 'requires one matching camera'
            profile = request('/setup/api/state')['cameras'][0]['profile']
            profile.update(camera=matches[0], serial=None, direct=direct, sdkFallback=fallback,
                           controls={'0': 100, '5': 50})
            request('/setup/api/cameras/0', profile)
            camera('connected', {'Connected': 'true'})
            initial = diagnostics()['values']
            previous = {int(k): initial[k] for k in ('16', '17', '21', '22', '23') if k in initial}
            assert all(k in previous for k in (16, 17, 21)), 'requires cooler and dew controls'
            row = {'mode': mode, 'auxiliary': [], 'samples': []}
            results['modes'].append(row)
            try:
                attach(diagnostics()['processId'])
                control(17, 0)
                time.sleep(3)
                # Restore full fan before enabling the cooler.
                for kind, values in ((22, (0, 128, 255)), (23, (0, 128, 255)), (21, (1, 0, 1))):
                    if kind not in previous:
                        continue
                    for value in values:
                        start = len(records)
                        control(kind, value)
                        wait_for(lambda: wire_check(kind, value, start), 10,
                                 f'{mode} control {kind}={value} did not reach hardware')
                        row['auxiliary'].append({'control': kind, 'value': value, 'usbVerified': True})
                # Use non-default auxiliary values to test restoration too.
                if 23 in previous:
                    control(23, 128)
                baseline = camera('ccdtemperature')
                target = max(10, min(20, math.floor(baseline - 10)))
                control(16, target)
                control(17, 1)
                deadline = time.monotonic() + 120
                while True:
                    time.sleep(2)
                    sample = {'temperatureC': camera('ccdtemperature'), 'powerPercent': camera('coolerpower')}
                    row['samples'].append(sample)
                    print(mode, 'cooling', sample, flush=True)
                    if sample['powerPercent'] >= 15 and sample['temperatureC'] <= baseline - 1:
                        break
                    assert time.monotonic() < deadline, 'no measured cooling response'
                assert sample['temperatureC'] > target + 3, 'must interrupt before the setpoint'
                camera('numx', {'NumX': 256})
                camera('numy', {'NumY': 256})
                before = diagnostics()
                row.update(baselineTemperatureC=baseline, priorTemperatureC=before['values']['8'] / 10,
                           priorPowerPercent=before['values']['15'], targetC=target)
                log_start = (args.output / 'server.log').stat().st_size
                camera('startexposure', {'Duration': 5, 'Light': 'false'})
                wait_for(lambda: camera('camerastate') == 2, 10, 'exposure did not start')
                time.sleep(.3)
                os.kill(before['processId'], signal.SIGTERM)
                started = time.monotonic()
                traced_pid = before['processId']
                while not camera('imageready'):
                    status = diagnostics()
                    if status['processId'] and status['processId'] != traced_pid:
                        attach(status['processId'])
                        traced_pid = status['processId']
                    assert time.monotonic() - started < 330, 'recovery deadline'
                    time.sleep(.5)
                frame = camera('imagearray', binary=True)
                assert len(frame) == 44 + 256 * 256 * 2
                assert struct.unpack('<11I', frame[:44])[1] == 0
                after = diagnostics()
                assert after['processId'] != before['processId']
                assert after['backend'] == ('sdk' if fallback or not direct else 'direct')
                for kind, value in {16: target, 17: 1, 21: 1, 22: 255, 23: 128}.items():
                    if kind in previous:
                        assert after['values'][str(kind)] == value, (kind, after['values'])
                events = []
                for line in (args.output / 'server.log').read_bytes()[log_start:].decode().splitlines():
                    if line.startswith('ZWOGAIN_DIAGNOSTIC '):
                        event = json.loads(line.split(' ', 1)[1])
                        if event['event'].startswith(('cooling.', 'capture.', 'backend.')):
                            events.append({'event': event['event'], 'message': event['message']})
                assert any(e['event'] == 'cooling.recovered' for e in events), events
                row.update(recoverySeconds=time.monotonic() - started, finalTemperatureC=camera('ccdtemperature'),
                           finalPowerPercent=camera('coolerpower'), restoredSetpointC=camera('setccdtemperature'),
                           recoveredBackend=after['backend'], restoredControls={str(k): after['values'][str(k)] for k in previous},
                           frameSha256=hashlib.sha256(frame[44:]).hexdigest(), events=events)
                assert row['finalTemperatureC'] > target + 2, 'test reached setpoint before recovery'
                print('RECOVERED', mode, json.dumps({k: row[k] for k in (
                    'priorTemperatureC', 'targetC', 'finalTemperatureC',
                    'finalPowerPercent', 'recoverySeconds', 'recoveredBackend')}), flush=True)
            finally:
                # Apply restoration while idle before disconnecting. In the
                # ordinary test starting state cooling is off, fan/LED are 255.
                try:
                    if camera('camerastate') != 0:
                        camera('abortexposure', {})
                        wait_for(lambda: camera('camerastate') == 0, 30, 'abort cleanup deadline')
                    control(17, 0)
                    time.sleep(3)
                    for kind in (16, 21, 22, 23, 17):
                        if kind in previous:
                            control(kind, previous[kind])
                    time.sleep(4)
                    row['restoredStartingControls'] = {str(k): diagnostics()['values'][str(k)] for k in previous}
                finally:
                    camera('connected', {'Connected': 'false'})
    finally:
        # Also release the camera if capability inspection failed before the
        # per-mode restoration block was entered.
        try:
            camera('connected', {'Connected': 'false'})
        except (OSError, RuntimeError):
            pass
        process.kill()
        process.wait()
        for session, script in traces:
            try:
                session.detach()
            except frida.InvalidOperationError:
                pass
        server_log.close()
        trace_file.close()
        (args.output / 'results.json').write_text(json.dumps(results, indent=2) + '\n')


if __name__ == '__main__':
    main()
