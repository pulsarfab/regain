"""Run Alpaca rotator contracts using the production CAA coordinator and simulated HID."""
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
    args = parser.parse_args()
    binary = Path(args.bin_dir).resolve() / ('zwogain-alpaca.exe' if os.name == 'nt' else 'zwogain-alpaca')
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    base = f'http://127.0.0.1:{port}'

    def request(path, data=None, method=None, origin=None):
        headers = {}
        if origin:
            headers['Origin'] = origin
        if data is not None:
            setup = path.startswith('/setup/')
            headers['Content-Type'] = 'application/json' if setup else 'application/x-www-form-urlencoded'
            data = (json.dumps(data) if setup else urllib.parse.urlencode(data)).encode()
        with urllib.request.urlopen(urllib.request.Request(base + path, data, headers, method=method), timeout=30) as response:
            return json.load(response)

    def rotator(member, values=None, client=1, error=0):
        query = {'ClientID': client, 'ClientTransactionID': 27}
        if values is not None:
            query.update(values)
        path = '/api/v1/rotator/0/' + member
        result = request(path + ('?' + urllib.parse.urlencode(query) if values is None else ''),
                         query if values is not None else None, 'GET' if values is None else 'PUT')
        assert result['ErrorNumber'] == error and result['ClientTransactionID'] == 27, result
        return result.get('Value')

    with tempfile.TemporaryDirectory(prefix='zwogain-rotator-') as directory:
        profile_path = Path(directory) / 'cameras.json'
        with open(Path(directory) / 'server.log', 'w+b') as log:
            process = subprocess.Popen([str(binary), '--simulate', '--no-discovery', '--port', str(port), '--profiles', str(profile_path)], stdout=log, stderr=log,
                                       creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
            try:
                deadline = time.monotonic() + 30
                while True:
                    try:
                        request('/setup/api/rotator')
                        break
                    except urllib.error.URLError:
                        assert process.poll() is None and time.monotonic() < deadline
                        time.sleep(.05)
                assert not request('/management/v1/configureddevices')['Value']
                assert rotator('interfaceversion') == 3
                rotator('position', error=0x407)
                rotator('connected', {'Connected': True}, error=0x40B)
                choices = request('/setup/api/rotator/discover', {})
                serial = choices[0]['identity']['serial']
                request('/setup/api/rotator', {'serial': serial})
                devices = request('/management/v1/configureddevices')['Value']
                assert len(devices) == 1 and devices[0]['DeviceType'] == 'Rotator' and devices[0]['DeviceNumber'] == 0
                identifier = devices[0]['UniqueID']
                try:
                    request('/setup/api/rotator', {'serial': None}, origin='https://untrusted.example')
                    raise AssertionError('Cross-origin setup write accepted')
                except urllib.error.HTTPError as error:
                    assert error.code == 403
                rotator('connected', {'Connected': True})
                rotator('connected', {'Connected': True}, client=2)
                rotator('connected', {'Connected': False})
                assert rotator('connected', client=2)
                rotator('position', error=0x407)
                rotator('connected', {'Connected': True})
                assert rotator('canreverse') and rotator('stepsize') == .02
                rotator('sync', {'Position': 42})
                assert abs(rotator('position') - 42) < .01
                rotator('reverse', {'Reverse': True})
                assert rotator('reverse') and abs(rotator('position') - 42) < .01
                rotator('movemechanical', {'Position': 153})
                assert not rotator('ismoving')
                assert abs(rotator('mechanicalposition') - 153) < .01
                assert abs(rotator('position') - 41) < .01
                rotator('moveabsolute', {'Position': 42})
                assert abs(rotator('mechanicalposition') - 152) < .01
                rotator('move', {'Position': 1})
                assert abs(rotator('position') - 43) < .01
                for invalid in [-1, 360, 'NaN']:
                    rotator('movemechanical', {'Position': invalid}, error=0x401)
                rotator('action', {'Action': 'bad', 'Parameters': ''}, error=0x40C)
                actions = rotator('supportedactions')
                assert 'ZwoGain.CAA.ResetOrigin' in actions
                def action(name, values=None):
                    return json.loads(rotator('action', {'Action': 'ZwoGain.CAA.' + name, 'Parameters': '' if values is None else json.dumps(values)}))
                before = rotator('position')
                action('ResetOrigin')
                assert rotator('mechanicalposition') == 0 and abs(rotator('position') - before) < .01
                action('SetReference', {'degrees': 152})
                action('SetLimit', {'degrees': 361})
                assert action('Status')['limit_degrees'] == 361
                action('SetBeep', {'enabled': False})
                assert not action('Settings')['beep']
                action('SetAlias', {'text': 'Test'})
                assert action('Identity')['alias'] == 'Test'
                action('RotateUnwrapped', {'degrees': 180})
                deadline = time.monotonic() + 10
                while rotator('ismoving'):
                    assert time.monotonic() < deadline
                    time.sleep(.1)
                assert not json.loads(profile_path.with_suffix('.rotator.json').read_text())['coordinatesUncertain']
                rotator('halt', {})
                rotator('connected', {'Connected': False}, client=2)
                rotator('connected', {'Connected': False})
                assert request('/management/v1/configureddevices')['Value'][0]['UniqueID'] == identifier
                saved = json.loads(profile_path.with_suffix('.rotator.json').read_text())
                rotator('connected', {'Connected': True})
                # The simulator starts at 152 each time; the persisted logical offset must be restored.
                assert abs(rotator('position') - ((152 + saved['logicalOffset']) % 360)) < .01
                rotator('connected', {'Connected': False})
                print('Alpaca CAA: discovery, selection, clients, motion, sync, reverse, actions, limits, multi-turn and persistence passed.')
            finally:
                process.terminate()
                process.wait(timeout=10)


if __name__ == '__main__':
    main()
