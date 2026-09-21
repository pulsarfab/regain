"""Dynamic focuser routing, independent sessions, stable IDs, and duplicate selection guards."""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin-dir', default='target/debug')
    args = parser.parse_args()
    binary = Path(args.bin_dir).resolve() / ('regain-alpaca.exe' if os.name == 'nt' else 'regain-alpaca')
    with tempfile.TemporaryDirectory(prefix='regain-focusers-') as directory:
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        base = f'http://127.0.0.1:{port}'
        command = [str(binary), '--simulate', '--no-discovery', '--port', str(port), '--profiles', str(Path(directory)/'profiles.json')]
        log = (Path(directory)/'server.log').open('w+b')
        def web(path, body=None, expected=200, origin=None):
            request = urllib.request.Request(base+path, None if body is None else json.dumps(body).encode(), {'Content-Type':'application/json', 'Origin':origin or base})
            try:
                response = urllib.request.urlopen(request, timeout=15)
            except urllib.error.HTTPError as error:
                assert error.code == expected, (path, error.code, error.read())
                return None
            assert response.status == expected, (path, response.status)
            return json.load(response)
        def api(slot, member, body=None, client=10, error=0):
            query = urllib.parse.urlencode({'ClientID':client, **(body or {})})
            url = base+f'/api/v1/focuser/{slot}/'+member
            request = urllib.request.Request(url+'?'+query) if body is None else urllib.request.Request(url, query.encode(), {'Content-Type':'application/x-www-form-urlencoded'}, method='PUT')
            value = json.load(urllib.request.urlopen(request, timeout=15))
            assert value['ErrorNumber'] == error, value
            return value.get('Value')
        def start():
            process = subprocess.Popen(command, stdout=log, stderr=log)
            for _ in range(200):
                try: web('/setup/api/focusers'); return process
                except OSError:
                    assert process.poll() is None
                    time.sleep(.05)
            process.kill(); process.wait()
            raise AssertionError('Server did not start')
        process = start()
        try:
            assert web('/setup/api/focusers') == []
            web('/setup/api/focusers', {'kind':'invalid'}, expected=400)
            web('/setup/api/focusers', {'kind':'eaf'}, expected=403, origin='http://unrelated.invalid')
            kinds = ['eta', 'fc3', 'fc3', 'eaf']
            profiles = []
            for slot, kind in enumerate(kinds):
                assert web('/setup/api/focusers', {'kind':kind})['slot'] == slot
                setup = f'/setup/api/focusers/{slot}'
                profile = web(setup)['profile']
                choices = web(setup+'/discover', {})
                profile['serial'] = choices[0]['identity']['serial'] if slot != 2 else '11:22:33:44:55:66'
                profile['label'] = f'My {kind} {slot}'
                web(setup, profile)
                profiles.append(profile)
                assert api(slot, 'name') == profile['label']
                api(slot, 'position', error=0x407)
            rows = web('/management/v1/configureddevices')['Value']
            assert [r['DeviceNumber'] for r in rows] == [0, 1, 2, 3], rows
            assert len({r['UniqueID'] for r in rows}) == 4
            original_ids = {r['slot']:r['profile']['uniqueId'] for r in web('/setup/api/focusers')}
            duplicate = dict(profiles[2], serial=profiles[1]['serial'].upper())
            web('/setup/api/focusers/2', duplicate, expected=400)
            web('/setup/api/focusers/0', profiles[0], expected=403, origin='http://unrelated.invalid')
            assert web('/setup/api/focusers/2')['profile']['serial'] == '11:22:33:44:55:66'
            web('/api/v1/focuser/999/name', expected=404)
            web('/setup/v1/focuser/999/setup', expected=404)
            for slot in [0, 1, 3]: api(slot, 'connected', {'Connected':True})
            # Shared clients belong to one slot only, even for two slots of the same model.
            api(1, 'connected', {'Connected':True}, client=11)
            api(1, 'connected', {'Connected':False})
            assert api(1, 'connected', client=11)
            assert not api(2, 'connected', client=11)
            api(2, 'position', client=11, error=0x407)
            web('/setup/api/focusers/1', profiles[1], expected=400)
            web('/setup/api/focusers/2/discover', {}, expected=400)
            api(1, 'connected', {'Connected':False}, client=11)
            # Clearing a selection disables advertising, not the slot or its identity.
            cleared = dict(profiles[1], serial=None)
            web('/setup/api/focusers/1', cleared)
            web('/setup/api/focusers/2', duplicate)
            api(2, 'connected', {'Connected':True})
            position = api(2, 'position')
            api(2, 'move', {'Position':position+10})
            deadline = time.monotonic()+10
            while api(2, 'ismoving'):
                assert time.monotonic() < deadline
                time.sleep(.05)
            assert api(2, 'position') == position+10
            assert not api(1, 'connected')
            for slot in [0, 2, 3]: api(slot, 'connected', {'Connected':False})
            process.terminate(); process.wait(timeout=10)
            # Restoring/copying a profile must not replace the slot's registry identity.
            profile_path = Path(directory)/'profiles.focuser-2.json'
            restored = json.loads(profile_path.read_text())
            restored['uniqueId'] = original_ids[0]
            profile_path.write_text(json.dumps(restored))
            process = start()
            reopened = web('/setup/api/focusers')
            assert {r['slot']:r['profile']['uniqueId'] for r in reopened} == original_ids
            assert [r['kind'] for r in reopened] == kinds
            rows = web('/management/v1/configureddevices')['Value']
            assert [r['DeviceNumber'] for r in rows] == [0, 2, 3]
            assert web('/setup/api/focusers', {'kind':'fc3'})['slot'] == 4
            print('Dynamic focusers: repeated models, stable IDs, restart, client isolation, duplicate guards and slot reassignment passed')
        except BaseException:
            log.flush(); log.seek(0); print(log.read().decode(errors='replace'))
            raise
        finally:
            if process.poll() is None: process.terminate(); process.wait(timeout=10)
            log.close()

if __name__ == '__main__':
    main()
