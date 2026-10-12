"""Exercise USB escalation through Alpaca, using full-frame RAW16 on physical cameras.

Hardware use requires an explicitly authorized idle camera and can disconnect
peripherals attached through its hub. Windows requires its already-prepared USB
helper; this test never prepares a helper or requests elevation.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import socket
import struct
import subprocess
import tempfile
import time
import urllib.parse
import urllib.request


HARDWARE_MODELS = {
    'ZWO ASI585MM Pro': 0x585e,
    'ZWO ASI676MC': 0x676d,
    'ZWO ASI662MC': 0x662b,
    'ZWO ASI2600MM Duo': 0x2601,
    'ZWO ASI2600MM Pro': 0x260e,
    'ZWO ASI6200MM Pro': 0x620b,
    'ZWO ASI220MM Mini': 0x2209,
}


def helper_authorization_ready(status, now):
    if 'expiresAtUnixSeconds' not in status:
        return False
    expiry = status['expiresAtUnixSeconds']
    if status.get('sessionScoped') is True:
        lifetime_ready = expiry is None
    else:
        legacy = 'sessionScoped' not in status or status['sessionScoped'] is False
        lifetime_ready = (legacy and type(expiry) is int
                          and now + 300 < expiry <= 253402300799)
    return (type(status.get('protocolVersion')) is int and status['protocolVersion'] == 1
            and status.get('running') is True
            and status.get('elevated') is True and status.get('approved') is True
            and lifetime_ready)


def diagnostic_records(path):
    records = []
    for line in path.read_text(encoding='utf-8', errors='replace').splitlines():
        if line.startswith('REGAIN_DIAGNOSTIC '):
            try:
                records.append(json.loads(line[len('REGAIN_DIAGNOSTIC '):]))
            except ValueError:
                pass
    return records


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin-dir', default='target/debug')
    parser.add_argument('--hardware-serial', help='Explicit authorization to reset this idle camera and capture full frames; '
                        'attached hub peripherals may disconnect. Windows requires an already-prepared helper.')
    parser.add_argument('--hardware-model', choices=HARDWARE_MODELS, default='ZWO ASI676MC',
                        help='Exact verified model for --hardware-serial (default: ZWO ASI676MC).')
    parser.add_argument('--evidence', type=Path)
    args = parser.parse_args()
    if args.hardware_serial is not None:
        args.hardware_serial = args.hardware_serial.lower()
        if (len(args.hardware_serial) != 16 or args.hardware_serial == '0000000000000000'
                or any(c not in '0123456789abcdef' for c in args.hardware_serial)):
            parser.error('--hardware-serial requires the exact nonzero 16-character hexadecimal serial')
    binary = Path(args.bin_dir).resolve()/('regain-alpaca.exe' if os.name == 'nt' else 'regain-alpaca')
    helper = binary.with_name('regain-device.exe' if os.name == 'nt' else 'regain-device')
    rejected = subprocess.run([str(helper),'usb','reset','invalid'],capture_output=True,text=True,timeout=10)
    assert rejected.returncode != 0
    if os.name == 'nt':
        token = json.dumps(dict(vendor=0x03c3,product=0x676d,serial='1234567890abcdef',location='USB\\VID_03C3&PID_676D\\TEST',address=None,generation=None)).encode().hex()
        expired = subprocess.run([str(helper),'usb','reset',token,'--elevated','0'],capture_output=True,text=True,timeout=10)
        assert expired.returncode != 0 and 'expired' in expired.stderr
    def bind_hardware():
        if args.hardware_serial and os.name == 'nt':
            # This reads the selected camera identity and closes its handle. The
            # helper must already be approved; there is deliberately no prepare
            # command or elevation fallback in this hardware test.
            bound = subprocess.run([str(helper), 'zwo', 'camera-direct', '--usb-target',
                                    args.hardware_model, args.hardware_serial],
                                   capture_output=True, text=True, timeout=15)
            if bound.returncode != 0:
                raise RuntimeError('Cannot bind the authorized idle camera before the USB test: ' + bound.stderr.strip())
            token = bound.stdout.strip()
            target = json.loads(bytes.fromhex(token))
            assert target['vendor'] == 0x03c3 and target['product'] == HARDWARE_MODELS[args.hardware_model]
            assert target['serial'].lower() == args.hardware_serial and target['generation']
            ready = subprocess.run([str(helper), 'usb', 'helper', 'status', token],
                                   capture_output=True, text=True, timeout=8)
            if ready.returncode != 0:
                raise RuntimeError('Prepare and check the USB reset helper for this camera before running the hardware test; '
                                   'the test never requests elevation. ' + ready.stderr.strip())
            status = json.loads(ready.stdout)
            if not helper_authorization_ready(status, time.time()):
                raise RuntimeError('The camera needs a prepared, approved USB reset helper for this Windows session '
                                   'or a legacy helper with at least five minutes of authorization remaining. '
                                   'Prepare it before running; no elevation is attempted here.')
            return target, status
        return None, None
    results = []
    report = dict(passed=False, hardwareModel=args.hardware_model if args.hardware_serial else None,
                  hardwareSerial=args.hardware_serial, cases=results)
    evidence_directory = None
    if args.evidence:
        args.evidence = args.evidence.resolve()
        args.evidence.parent.mkdir(parents=True, exist_ok=True)
        if args.evidence.exists():
            parser.error('--evidence must name a new file; existing evidence is never overwritten')
        evidence_directory = args.evidence.parent / (args.evidence.stem + '-logs')
        evidence_directory.mkdir()
    def save_evidence():
        if args.evidence:
            args.evidence.write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
    save_evidence()
    for direct in [True, False]:
        backend = 'direct' if direct else 'sdk'
        row = dict(backend=backend, simulation=not bool(args.hardware_serial), passed=False, frames=[])
        results.append(row)
        save_evidence()
        temporary = tempfile.TemporaryDirectory(prefix='regain-usb-reset-') if evidence_directory is None else None
        try:
            directory = Path(temporary.name) if temporary else evidence_directory / backend
            if not temporary:
                directory.mkdir()
            row['log'] = str(directory / 'server.log') if evidence_directory else None
            target, helper_status = bind_hardware()
            row.update(usbBindingBefore=target, helperBefore=helper_status)
            with socket.socket() as sock:
                sock.bind(('127.0.0.1', 0)); port = sock.getsockname()[1]
            base = f'http://127.0.0.1:{port}'
            def request(path, data=None, method=None, binary=False):
                headers = {}
                if data is not None:
                    setup = path.startswith('/setup/')
                    headers['Content-Type'] = 'application/json' if setup else 'application/x-www-form-urlencoded'
                    data = (json.dumps(data) if setup else urllib.parse.urlencode(data)).encode()
                if binary: headers['Accept'] = 'application/imagebytes'
                with urllib.request.urlopen(urllib.request.Request(base+path, data, headers, method=method), timeout=90) as response:
                    return response.read() if binary else json.load(response)
            def camera(member, data=None, binary=False):
                value = request('/api/v1/camera/0/'+member+'?ClientID=765', None if data is None else dict(data,ClientID=765), 'PUT' if data is not None else 'GET', binary)
                if binary: return value
                assert value['ErrorNumber'] == 0, value
                return value.get('Value')
            def diagnostics():
                return json.loads(camera('action', {'Action':'Regain.Diagnostics', 'Parameters':''}))
            log_path = directory / 'server.log'
            with open(log_path,'w+b') as log:
                command = [str(binary), '--no-discovery', '--port', str(port), '--profiles', str(Path(directory)/'profiles.json')]
                if not args.hardware_serial: command += ['--simulate']
                else: command += ['--sdk', str(Path('vendor/zwo/ASICamera2.dll').resolve())]
                process = subprocess.Popen(command, stdout=log, stderr=log)
                try:
                    deadline = time.monotonic()+20
                    while True:
                        try: state=request('/setup/api/state'); break
                        except OSError:
                            assert process.poll() is None and time.monotonic()<deadline
                            time.sleep(.05)
                    devices = request('/setup/api/discover', {'direct':direct})
                    name = args.hardware_model if args.hardware_serial else ('ZWO ASI676MC' if direct else 'ZWO Simulated')
                    matching = [d for d in devices if d['name'] == name]
                    assert len(matching) == 1, matching
                    descriptor = matching[0]
                    profile = state['cameras'][0]['profile']
                    profile.update(camera=descriptor, direct=direct, sdkFallback=False, serial=args.hardware_serial)
                    profile['recovery'].update(usbResetAfterFailures=1, maxRetries=1, maximumRetryExposureSeconds=10,
                        reconnectDelaySeconds=.1, readyFrameDownloadRetries=0, directReadRetries=0)
                    request('/setup/api/cameras/0', profile)
                    camera('connected', {'Connected':True})
                    before = diagnostics()
                    row['before'] = before
                    pid = before['processId']
                    assert pid and pid != process.pid
                    width, height = (int(descriptor['width']), int(descriptor['height'])) if args.hardware_serial else (64, 64)
                    assert width > 0 and height > 0
                    if args.hardware_serial:
                        assert camera('cameraxsize') == width and camera('cameraysize') == height
                    camera('binx', {'BinX':1}); camera('biny', {'BinY':1})
                    camera('startx', {'StartX':0}); camera('starty', {'StartY':0})
                    camera('numx', {'NumX':width}); camera('numy', {'NumY':height})
                    camera('readoutmode', {'ReadoutMode':0})
                    exposure_seconds = 5 if args.hardware_serial else 2
                    def download_frame(label, already_started=False):
                        started = time.monotonic()
                        if not already_started:
                            camera('startexposure', {'Duration':exposure_seconds, 'Light':False})
                        deadline = time.monotonic() + 150
                        while not camera('imageready'):
                            assert time.monotonic() < deadline, diagnostics()
                            time.sleep(.1)
                        ready = time.monotonic()
                        frame = camera('imagearray', binary=True)
                        pixel_bytes = width * height * 2
                        assert len(frame) == 44 + pixel_bytes
                        header = struct.unpack('<11I', frame[:44])
                        assert header[0:2] == (1, 0) and header[4:] == (44, 2, 8, 2, width, height, 0), header
                        details = dict(label=label, width=width, height=height, bytes=pixel_bytes,
                                       bin=1, format='RAW16', fullFrame=bool(args.hardware_serial),
                                       exposureSeconds=exposure_seconds,
                                       readyAfterSeconds=ready-started,
                                       deliveredAfterSeconds=time.monotonic()-started,
                                       sha256=hashlib.sha256(memoryview(frame)[44:]).hexdigest(),
                                       diagnostics=diagnostics())
                        row['frames'].append(details)
                        save_evidence()
                        return details
                    baseline = download_frame('baseline')
                    assert baseline['diagnostics']['processId'] == pid, baseline
                    assert baseline['diagnostics']['retry']['recaptures'] == 0, baseline
                    log.flush()
                    assert not [r for r in diagnostic_records(log_path)
                                if r.get('pid') == process.pid and r.get('event') == 'usb.reset']
                    camera('startexposure', {'Duration':exposure_seconds, 'Light':False})
                    deadline = time.monotonic()+10
                    while diagnostics()['phase'] != 'Exposing':
                        assert time.monotonic()<deadline
                        time.sleep(.01)
                    # Kill only the worker PID returned by our isolated server.
                    os.kill(pid, signal.SIGTERM)
                    replacement = download_frame('replacement-after-usb-recovery', already_started=True)
                    after = diagnostics()
                    assert after['serial'] == before['serial'] and after['processId'] != pid
                    row['afterRecovery'] = after
                    row.update(workerTerminated=True, sameSerial=True, replacementFrame=replacement)
                    for label in ('fresh-after-recovery-1', 'fresh-after-recovery-2'):
                        fresh = download_frame(label)
                        assert fresh['diagnostics']['processId'] == after['processId'], fresh
                        assert fresh['diagnostics']['serial'] == before['serial'], fresh
                        assert fresh['diagnostics']['retry']['recaptures'] == 0, fresh
                    if args.hardware_serial:
                        assert len({frame['sha256'] for frame in row['frames']}) == len(row['frames']), \
                            'Physical baseline, replacement and fresh frames must have distinct pixel hashes'
                    log.flush()
                    records = diagnostic_records(log_path)
                    resets = [r for r in records if r.get('pid') == process.pid and r.get('event') == 'usb.reset']
                    assert len(resets) == 1, resets
                    row['usbResets'] = len(resets)
                    row['usbLifecycle'] = [r for r in records if r.get('pid') == process.pid
                                           and str(r.get('event', '')).startswith('usb.')]
                    if args.hardware_serial and os.name == 'nt':
                        helper_results = [json.loads(r['message']) for r in row['usbLifecycle']
                                          if r.get('event') == 'usb.helper.result']
                        assert len(helper_results) == 1, helper_results
                        native = helper_results[0]
                        assert native['nativeOperation'] == 'USB_HUB_CYCLE_PORT'
                        assert native['nativeOperationAcknowledged'] is True and native['deadlineExpired'] is False
                        assert native['returnedGeneration'] and native['returnedGeneration'] != target['generation'], native
                        assert native['target']['serial'].lower() == args.hardware_serial
                        row['nativeCycle'] = native
                    camera('connected', {'Connected':False})
                    rebound, helper_after = bind_hardware()
                    row.update(usbBindingAfter=rebound, helperAfter=helper_after)
                    if target:
                        assert rebound['generation'] != target['generation'], rebound
                        assert all(rebound[k] == target[k] for k in ('vendor', 'product', 'serial', 'location'))
                    row['passed'] = True
                    print(f"{'Physical' if args.hardware_serial else 'Simulated'} {name} ({backend}): baseline, worker death, one USB reset, same-serial reconnect, replacement and two fresh RAW16 frames passed")
                except BaseException as error:
                    row['error'] = repr(error)
                    log.flush(); log.seek(max(0, log.tell()-30000)); print(log.read().decode(errors='replace'))
                    raise
                finally:
                    try:
                        if camera('connected'):
                            camera('abortexposure', {})
                            camera('connected', {'Connected':False})
                    except Exception as error:
                        row['cleanupError'] = repr(error)
                    process.terminate()
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)
        except BaseException as error:
            row.setdefault('error', repr(error))
            raise
        finally:
            save_evidence()
            if temporary:
                temporary.cleanup()
    report['passed'] = all(row['passed'] for row in results)
    save_evidence()


if __name__ == '__main__': main()
