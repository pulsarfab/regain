"""Exercise USB escalation through Alpaca; hardware requires an explicit ASI676 serial."""
import argparse
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin-dir', default='target/debug')
    parser.add_argument('--hardware-serial', help='Explicit authorization to reset this idle ASI676')
    parser.add_argument('--evidence', type=Path)
    args = parser.parse_args()
    binary = Path(args.bin_dir).resolve()/('regain-alpaca.exe' if os.name == 'nt' else 'regain-alpaca')
    helper = binary.with_name('regain-device.exe' if os.name == 'nt' else 'regain-device')
    rejected = subprocess.run([str(helper),'usb','reset','invalid'],capture_output=True,text=True,timeout=10)
    assert rejected.returncode != 0
    if os.name == 'nt':
        token = json.dumps(dict(vendor=0x03c3,product=0x676d,serial='1234567890abcdef',location='USB\\VID_03C3&PID_676D\\TEST',address=None,generation=None)).encode().hex()
        expired = subprocess.run([str(helper),'usb','reset',token,'--elevated','0'],capture_output=True,text=True,timeout=10)
        assert expired.returncode != 0 and 'expired' in expired.stderr
    results = []
    for direct in [True, False]:
        with tempfile.TemporaryDirectory(prefix='regain-usb-reset-') as directory:
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
            with open(Path(directory)/'server.log','w+b') as log:
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
                    name = 'ZWO ASI676MC' if direct or args.hardware_serial else 'ZWO Simulated'
                    descriptor = next(d for d in devices if d['name']==name)
                    profile = state['cameras'][0]['profile']
                    profile.update(camera=descriptor, direct=direct, sdkFallback=False, serial=args.hardware_serial)
                    profile['recovery'].update(usbResetAfterFailures=1, maxRetries=1, maximumRetryExposureSeconds=10,
                        reconnectDelaySeconds=.1, readyFrameDownloadRetries=0, directReadRetries=0)
                    request('/setup/api/cameras/0', profile)
                    camera('connected', {'Connected':True})
                    before = diagnostics()
                    pid = before['processId']
                    assert pid and pid != process.pid
                    camera('numx', {'NumX':64}); camera('numy', {'NumY':64})
                    camera('startexposure', {'Duration':2, 'Light':False})
                    deadline = time.monotonic()+10
                    while diagnostics()['phase'] != 'Exposing':
                        assert time.monotonic()<deadline
                        time.sleep(.01)
                    # Kill only the worker PID returned by our isolated server.
                    os.kill(pid, signal.SIGTERM)
                    deadline = time.monotonic()+150
                    while not camera('imageready'):
                        assert time.monotonic()<deadline, diagnostics()
                        time.sleep(.1)
                    frame = camera('imagearray', binary=True)
                    assert len(frame) == 44+8192
                    assert struct.unpack('<11I',frame[:44])[8:10] == (64,64)
                    after = diagnostics()
                    assert after['serial'] == before['serial'] and after['processId'] != pid
                    events = request('/setup/api/state')['logs']
                    resets = [e for e in events if 'Resetting the selected camera;' in e]
                    assert len(resets) == 1, events
                    camera('connected', {'Connected':False})
                    results.append(dict(backend='direct' if direct else 'sdk', simulation=not bool(args.hardware_serial),
                        workerTerminated=True, usbResets=1, sameSerial=True, replacementFrame={'width':64,'height':64,'bytes':8192}))
                    print(f"{'Physical' if args.hardware_serial else 'Simulated'} {name} ({results[-1]['backend']}): worker death, USB reset, same-serial reconnect and replacement RAW16 frame passed")
                except BaseException:
                    log.flush(); log.seek(0); print(log.read().decode(errors='replace'))
                    raise
                finally:
                    process.terminate(); process.wait(timeout=10)
    if args.evidence:
        args.evidence.write_text(json.dumps({'cases':results},indent=2)+'\n',encoding='utf-8')


if __name__ == '__main__': main()
