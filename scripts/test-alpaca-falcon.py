"""Falcon integration and persistent multi-model rotator slots; opt in to physical motion."""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.request
import urllib.parse
import urllib.error


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin-dir', default='target/debug')
    parser.add_argument('--short', action='store_true', help='Skip completed long-travel validation when rechecking other changes')
    parser.add_argument('--hardware-serial', help='Explicit selection; authorizes motion and origin reset')
    args = parser.parse_args()
    binary = Path(args.bin_dir).resolve() / ('regain-alpaca.exe' if os.name == 'nt' else 'regain-alpaca')
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    base = f'http://127.0.0.1:{port}'

    def request(path, data=None, method=None):
        headers = {}
        if data is not None:
            setup = path.startswith('/setup/')
            headers['Content-Type'] = 'application/json' if setup else 'application/x-www-form-urlencoded'
            data = (json.dumps(data) if setup else urllib.parse.urlencode(data)).encode()
        with urllib.request.urlopen(urllib.request.Request(base+path, data, headers, method=method), timeout=30) as response:
            return json.load(response)

    def rotator(member, data=None, client=1, error=0, slot=0):
        p = {'ClientID': client, 'ClientTransactionID': 29}
        if data is not None:
            p.update(data)
        path = f'/api/v1/rotator/{slot}/{member}'
        result = request(path + ('?'+urllib.parse.urlencode(p) if data is None else ''),
                         p if data is not None else None, 'GET' if data is None else 'PUT')
        assert result['ErrorNumber'] == error and result['ClientTransactionID'] == 29, result
        return result.get('Value')

    def action(name, values=None):
        return json.loads(rotator('action', {'Action': 'Regain.Falcon.'+name, 'Parameters': json.dumps(values or {})}))

    def idle():
        deadline = time.monotonic()+900
        while rotator('ismoving'):
            assert time.monotonic() < deadline, 'Motion timeout'
            time.sleep(.1)

    def near(a, b):
        assert abs((a-b+180) % 360-180) < .06, (a, b)

    with tempfile.TemporaryDirectory(prefix='regain-falcon-') as directory:
        profile = Path(directory)/'cameras.json'
        command = [str(binary), '--no-discovery', '--port', str(port), '--profiles', str(profile)]
        if not args.hardware_serial:
            command.append('--simulate')
        with open(Path(directory)/'server.log','w+b') as log:
            def start():
                proc = subprocess.Popen(command, stdout=log, stderr=log,
                                        creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
                deadline = time.monotonic()+30
                while True:
                    try:
                        request('/setup/api/rotators')
                        return proc
                    except urllib.error.URLError:
                        assert proc.poll() is None and time.monotonic() < deadline
                        time.sleep(.05)
            proc = start()
            try:
                assert request('/setup/api/rotators') == []
                for kind, expected in [('falcon',0),('caa',1),('falcon',2)]:
                    assert request('/setup/api/rotators', {'kind':kind})['slot'] == expected
                rows = request('/setup/api/rotators')
                ids = [r['profile']['uniqueId'] for r in rows]
                assert len(set(ids)) == 3
                choices = request('/setup/api/rotators/0/discover', {})
                serial = args.hardware_serial or choices[0]['identity']['serial']
                assert any(r['identity']['serial'] == serial for r in choices)
                request('/setup/api/rotators/0', {'serial':serial})
                try:
                    request('/setup/api/rotators/2', {'serial':serial})
                    raise AssertionError('Duplicate physical selection accepted')
                except urllib.error.HTTPError:
                    pass
                assert request('/management/v1/configureddevices')['Value'][0]['UniqueID'] == ids[0]
                rotator('position',error=0x407)
                rotator('connected',{'Connected':True})
                rotator('connected',{'Connected':True},client=2)
                rotator('connected',{'Connected':False})
                assert rotator('connected',client=2)
                rotator('connected',{'Connected':True})
                for endpoint in ['/setup/api/rotators/0/discover', '/setup/api/rotators/2/discover']:
                    try:
                        request(endpoint,{})
                        raise AssertionError('Scan accepted while another client owns the model')
                    except urllib.error.HTTPError:
                        pass
                assert rotator('stepsize') == .01
                assert 'Regain.Falcon.ResetOrigin' in rotator('supportedactions')
                assert all('SetLimit' not in a for a in rotator('supportedactions'))
                rotator('reverse',{'Reverse':False})
                action('ResetOrigin')
                rotator('sync',{'Position':42})
                rotator('movemechanical',{'Position':5}); idle()
                near(rotator('position'),47)
                rotator('reverse',{'Reverse':True})
                near(rotator('position'),47)
                rotator('moveabsolute',{'Position':48}); idle()
                near(rotator('mechanicalposition'),6)
                rotator('reverse',{'Reverse':False})
                before = rotator('position')
                action('ResetOrigin'); near(rotator('position'),before)
                for travel in ([] if args.short else [450,-450]):
                    before = rotator('position')
                    print(f'Starting physical travel {travel} degrees' if args.hardware_serial else f'Simulated travel {travel}', flush=True)
                    action('RotateUnwrapped',{'degrees':travel}); idle()
                    print(f'Completed travel {travel}',flush=True)
                    near(rotator('position'),before+travel)
                action('RotateUnwrapped',{'degrees':450})
                rotator('halt',{}); assert not rotator('ismoving')
                action('ResetOrigin'); rotator('sync',{'Position':0})
                near(rotator('mechanicalposition'),0)
                rotator('connected',{'Connected':False},client=2)
                rotator('connected',{'Connected':False})
                for _ in range(3):
                    rotator('connected',{'Connected':True})
                    near(rotator('position'),0)
                    rotator('connected',{'Connected':False})
                proc.terminate(); proc.wait(timeout=10)
                proc = start()
                assert [r['profile']['uniqueId'] for r in request('/setup/api/rotators')] == ids
                assert request('/setup/api/rotators/0')['profile']['serial'] == serial
                print('Falcon Alpaca: discovery, leases, coordinate transforms, ' + ('short travel' if args.short else '+/-450 travel') + ', halt, reset, reconnect and stable slots passed.')
            finally:
                try:
                    rotator('halt',{})
                    rotator('connected',{'Connected':False},client=2)
                    rotator('connected',{'Connected':False})
                except Exception:
                    pass
                proc.terminate(); proc.wait(timeout=10)


if __name__ == '__main__':
    main()
