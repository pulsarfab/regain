"""FocusCube3 worker and Alpaca integration. Hardware tests restore position/settings."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.parse
import urllib.request

# Reuse the bounded JSON-pipe client used for OFP2.
spec = importlib.util.spec_from_file_location('ofp2_test', Path(__file__).with_name('test-ofp2.py'))
helpers = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helpers)

def wait(call, target):
    deadline = time.monotonic() + 20
    while True:
        state = call('status')
        if not state['moving']:
            assert state['position'] == target, state
            return state
        assert time.monotonic() < deadline
        time.sleep(.1)

def exercise(call):
    original = call('status')
    assert not original['moving']
    start = original['position']
    target = start + 50 if start <= 999950 else start - 50
    try:
        call('move', position=target)
        wait(call, target)
        call('move', position=start)
        wait(call, start)
        call('settings', speed=50, backlash=0, reverse=not original['reverse'])
        changed = call('status')
        assert changed['speed'] == 50 and changed['backlash'] == 0 and changed['reverse'] != original['reverse']
        call('move', position=target)
        call('halt')
        halted = call('status')
        assert not halted['moving']
    finally:
        call('halt')
        call('settings', speed=original['speed'], backlash=original['backlash'], reverse=original['reverse'])
        call('move', position=start)
        restored = wait(call, start)
        for key in ('position', 'speed', 'backlash', 'reverse'):
            assert restored[key] == original[key], (key, original, restored)
    return {'initial': original, 'halted': halted, 'restored': restored}

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--bin-dir', default='target/debug')
    p.add_argument('--hardware', action='store_true')
    p.add_argument('--serial')
    p.add_argument('--report', type=Path)
    a = p.parse_args()
    if a.hardware and not a.serial: p.error('--hardware requires --serial')
    serial = a.serial if a.hardware else '00:00:00:00:00:03'
    sim = [] if a.hardware else ['--simulate']
    directory = Path(a.bin_dir).resolve()
    def binary(name): return str(directory / (name + ('.exe' if os.name == 'nt' else '')))
    devices = json.loads(subprocess.check_output([binary('regain-fc3'), 'list-details', *sim], timeout=30))
    assert sum(d['identity']['serial'].lower() == serial.lower() for d in devices) == 1, devices
    report = {'hardware': a.hardware}
    worker = helpers.Worker(binary('regain-fc3'), serial, sim)
    try:
        identity = worker.call('identity')
        report['identity'] = {k: v for k, v in identity.items() if k not in ('serial','port')}
        for command, values in [('move', {'position': -1}), ('move', {'position': 1000001}), ('settings', {'speed': 0}), ('settings', {'speed': 1}), ('settings', {'speed': 399}), ('settings', {'backlash': 1001}), ('settings', {'reverse': 'true'}), ('settings', {'beep': True})]:
            worker.call(command, expected_error=True, **values)
        report['worker'] = exercise(worker.call)
    finally: worker.close()
    with tempfile.TemporaryDirectory(prefix='fc3-alpaca-') as tmp:
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0)); port = sock.getsockname()[1]
        server = subprocess.Popen([binary('regain-alpaca'), '--port', str(port), '--workers', str(directory), '--profiles', str(Path(tmp)/'profiles.json'), '--no-discovery', *sim], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        base = f'http://127.0.0.1:{port}'
        def web(path, data=None, method=None):
            encoded = None if data is None else json.dumps(data).encode()
            headers = {'Content-Type': 'application/json', 'Origin': base}
            return json.load(urllib.request.urlopen(urllib.request.Request(base+path, encoded, headers, method=method), timeout=20))
        def api(member, values=None, client=700):
            query = urllib.parse.urlencode({'ClientID': client, **(values or {})})
            url = base+'/api/v1/focuser/1/'+member
            req = urllib.request.Request(url+'?'+query) if values is None else urllib.request.Request(url, query.encode(), {'Content-Type':'application/x-www-form-urlencoded'}, method='PUT')
            r = json.load(urllib.request.urlopen(req, timeout=20))
            assert r['ErrorNumber'] == 0, r
            return r.get('Value')
        try:
            for _ in range(100):
                try: state=web('/setup/api/accessory/fc3'); break
                except OSError: time.sleep(.1)
            else: raise AssertionError('Alpaca did not start')
            state['profile']['serial']=serial
            web('/setup/api/accessory/fc3',state['profile'])
            configured=web('/management/v1/configureddevices')['Value']
            assert any(d['DeviceType']=='Focuser' and d['DeviceNumber']==1 for d in configured), configured
            api('connected',{'Connected':True}); api('connected',{'Connected':True},701)
            assert json.loads(api('action',{'Action':'ZwoGain.Identity','Parameters':''}))['model'] == 'Pegasus Astro FocusCube3'
            api('connected',{'Connected':False})
            assert api('connected',client=701)
            api('connected',{'Connected':True})
            def call(command, **values):
                if command=='status': return json.loads(api('action',{'Action':'Regain.Status','Parameters':''}))
                if command=='settings': return web('/setup/api/accessory/fc3/settings',values)
                return api(command, {'Position': values['position']} if command=='move' else {})
            report['alpaca']=exercise(call)
            api('connected',{'Connected':False}); api('connected',{'Connected':False},701)
        finally:
            server.terminate(); server.wait(timeout=10)
    if a.report:
        a.report.parent.mkdir(parents=True,exist_ok=True)
        a.report.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
    print('FocusCube3 worker and Alpaca: movement, halt, validation, settings restoration, client sharing passed')

if __name__ == '__main__': main()
