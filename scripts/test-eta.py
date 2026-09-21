"""ETA worker and Alpaca tests. Physical mode reads only; movement tests use simulation."""
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

spec = importlib.util.spec_from_file_location('helpers', Path(__file__).with_name('test-ofp2.py'))
helpers = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helpers)

def wait(status, target):
    deadline = time.monotonic() + 15
    while True:
        s = status()
        assert not s['fault'], s
        if not s['moving']:
            assert abs(s['position'] - target) <= 2, s
            return s
        assert time.monotonic() < deadline, s
        time.sleep(.1)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin-dir', default='target/debug')
    parser.add_argument('--read-only-port', help='Read a physical ETA without sending motor commands')
    a = parser.parse_args()
    directory = Path(a.bin_dir).resolve()
    def binary(name): return str(directory / (name + ('.exe' if os.name == 'nt' else '')))
    serial = a.read_only_port or 'SIMULATION'
    sim = [] if a.read_only_port else ['--simulate']
    worker = helpers.Worker([binary('regain-device'), 'wanderer', 'eta'], serial, sim)
    try:
        assert worker.call('identity')['model'] == 'Wanderer Astro ETA M54'
        if a.read_only_port:
            state = worker.call('status')
            assert not state['fault'] and len(state['points_um']) == 3, state
            print('Physical worker read-only:', json.dumps(state))
        else:
            for command, args in [('move', {'position': 0}), ('move', {'position': 1201}), ('move-point', {'point': 4, 'position': 10}), ('move-point', {'point': 1, 'position': -1}), ('halt', {})]:
                worker.call(command, expected_error=True, **args)
            worker.call('move', position=500)
            worker.call('move-point', point=1, position=300, expected_error=True)
            s = wait(lambda: worker.call('status'), 500)
            assert s['points_um'] == [490, 500, 510], s
            worker.call('move-point', point=2, position=530)
            wait(lambda: worker.call('status'), 510)
            worker.call('move', position=600)
            worker.call('cancel-queued')
            s = wait(lambda: worker.call('status'), 540)
            assert s['points_um'] == [580, 530, 510], s
    finally:
        worker.close()
    with tempfile.TemporaryDirectory(prefix='eta-alpaca-') as tmp:
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0)); port = sock.getsockname()[1]
        server = subprocess.Popen([binary('regain-alpaca'), '--port', str(port), '--workers', str(directory), '--profiles', str(Path(tmp)/'profiles.json'), '--no-discovery', *sim], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        base = f'http://127.0.0.1:{port}'
        def web(path, body=None):
            return json.load(urllib.request.urlopen(urllib.request.Request(base+path, None if body is None else json.dumps(body).encode(), {'Content-Type':'application/json', 'Origin':base}), timeout=10))
        def api(member, body=None, client=701, error=None):
            query = urllib.parse.urlencode({'ClientID':client, **(body or {})})
            url = base+f'/api/v1/focuser/{slot}/'+member
            req = urllib.request.Request(url+'?'+query) if body is None else urllib.request.Request(url, query.encode(), {'Content-Type':'application/x-www-form-urlencoded'}, method='PUT')
            r = json.load(urllib.request.urlopen(req, timeout=10))
            assert r['ErrorNumber'] == (error or 0), r
            return r.get('Value')
        def status(): return json.loads(api('action', {'Action':'Regain.Status','Parameters':''}))
        try:
            for _ in range(100):
                try: web('/setup/api/focusers'); break
                except OSError: time.sleep(.1)
            else: raise AssertionError('Server did not start')
            slot=web('/setup/api/focusers',{'kind':'eta'})['slot']
            setup=f'/setup/api/focusers/{slot}'
            state=web(setup)
            state['profile']['serial']=serial
            web(setup,state['profile'])
            assert any(d['DeviceType']=='Focuser' and d['DeviceNumber']==slot for d in web('/management/v1/configureddevices')['Value'])
            api('connected',{'Connected':True})
            api('connected',{'Connected':True},client=702)
            api('connected',{'Connected':False},client=702)
            assert api('connected')
            assert api('stepsize') == 1
            assert 'Regain.MovePoint' in api('supportedactions')
            if a.read_only_port:
                observed = status()
                assert not observed['fault'] and len(observed['points_um']) == 3, observed
                print('Physical Alpaca read-only:', json.dumps(observed))
            else:
                api('halt',{},error=0x400)
                api('move',{'Position':1201},error=0x401)
                api('action',{'Action':'Regain.MovePoint','Parameters':'{"point":4,"position":2}'},error=0x401)
                api('move',{'Position':500})
                assert wait(status,500)['points_um'] == [490,500,510]
                api('action',{'Action':'Regain.MovePoint','Parameters':'{"point":2,"position":530}'})
                wait(status,510)
            api('connected',{'Connected':False})
            api('position',error=0x407)
        finally:
            server.terminate(); server.wait(timeout=10)
    if a.read_only_port:
        print('ETA physical read-only worker, Alpaca and client sharing passed')
        return
    print('ETA simulation: sequential back focus, tilt, limits, cancellation, Alpaca routing and client sharing passed')

if __name__ == '__main__': main()
