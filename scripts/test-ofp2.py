"""Exercise native OFP2 and Alpaca CoverCalibrator contracts. Hardware opt-in restores state."""
import argparse
import json
import os
from pathlib import Path
import queue
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request


def exercise(call, label):
    original = call('status')
    assert original['cover'] in ('open', 'closed'), original

    def wait(expected=None):
        deadline = time.monotonic() + 125
        while True:
            state = call('status')
            if state['cover'] != 'moving':
                if expected is not None:
                    assert state['cover'] == expected, state
                return state
            assert time.monotonic() < deadline, state
            time.sleep(.15)

    try:
        for brightness in (0, 128, 4096):
            call('on', brightness=brightness)
            state = call('status')
            assert state['calibrator_on'] and state['brightness'] == brightness, state
            assert state['light_on'] == (brightness != 0), state
        call('off')
        state = call('status')
        assert not state['light_on'] and not state['calibrator_on'] and state['brightness'] == 0, state
        target = 'close' if original['cover'] == 'open' else 'open'
        call(target)
        assert call('status')['cover'] == 'moving'
        call('halt')
        halted = wait()
        assert halted['cover'] == 'unknown', halted
        assert 0 < halted['position_degrees'] < 270, halted
        call(target)
        wait('closed' if target == 'close' else 'open')
        call('open' if target == 'close' else 'close')
        wait(original['cover'])
        print(label + ': brightness 0/128/4096, off, full open/close, mid-travel halt/resume passed', flush=True)
    finally:
        call('halt')
        wait()
        call('open' if original['cover'] == 'open' else 'close')
        wait(original['cover'])
        if original['light_on']:
            call('on', brightness=original['brightness'])
        else:
            call('off')
        restored = call('status')
        assert restored['cover'] == original['cover']
        assert restored['light_on'] == original['light_on']
    return {'initial': original, 'halted': halted, 'restored': restored}


class Worker:
    def __init__(self, binary, serial, simulate):
        self.process = subprocess.Popen([str(binary), 'serve', '--serial', serial, *simulate],
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.lines = queue.Queue()
        def reader():
            for line in self.process.stdout:
                self.lines.put(line)
            self.lines.put(b'')
        threading.Thread(target=reader, daemon=True).start()
        self.first = True

    def call(self, command, expected_error=False, **values):
        data = json.dumps({'command': command, **values}) + '\n'
        if self.first:
            data = '\ufeff' + data
            self.first = False
        self.process.stdin.write(data.encode('utf-8'))
        self.process.stdin.flush()
        raw = self.lines.get(timeout=15)
        assert raw, 'Worker exited unexpectedly'
        reply = json.loads(raw)
        assert reply['ok'] != expected_error, reply
        return reply.get('result')

    def close(self):
        self.process.stdin.close()
        try:
            code = self.process.wait(timeout=8)
            assert code == 0, self.process.stderr.read().decode()
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin-dir', default='target/debug')
    parser.add_argument('--hardware', action='store_true')
    parser.add_argument('--serial', help='required USB serial when using hardware')
    parser.add_argument('--report', type=Path)
    args = parser.parse_args()
    if args.hardware and not args.serial:
        parser.error('--hardware requires --serial')
    serial = args.serial if args.hardware else 'SIM-OFP2'
    simulate = [] if args.hardware else ['--simulate']
    directory = Path(args.bin_dir).resolve()
    def binary(name):
        return directory / (name + ('.exe' if os.name == 'nt' else ''))
    devices = json.loads(subprocess.check_output([str(binary('zwogain-ofp2')), 'list-details', *simulate], timeout=30))
    assert sum(d['serial'] == serial for d in devices) == 1, devices
    worker = Worker(binary('zwogain-ofp2'), serial, simulate)
    report = {'hardware': args.hardware}
    try:
        identity = worker.call('identity')
        assert identity['serial'] == serial and identity['product'] == 3
        report['identity'] = {k: v for k, v in identity.items() if k not in ('serial', 'port')}
        for invalid in (-1, 4097, 1.5, '128'):
            worker.call('on', brightness=invalid, expected_error=True)
        worker.call('bad', expected_error=True)
        if args.hardware:
            second = subprocess.run([str(binary('zwogain-ofp2')), 'status', '--serial', serial],
                                    capture_output=True, timeout=10)
            assert second.returncode != 0, 'Serial ownership was not exclusive'
        report['native'] = exercise(worker.call, 'Rust worker')
    finally:
        worker.close()

    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    def request(path, data=None, method=None, origin=None):
        headers = {}
        if origin:
            headers.update(Origin=origin)
        if data is not None:
            setup = path.startswith('/setup/')
            headers['Content-Type'] = 'application/json' if setup else 'application/x-www-form-urlencoded'
            data = (json.dumps(data) if setup else urllib.parse.urlencode(data)).encode()
        with urllib.request.urlopen(urllib.request.Request(base+path, data, headers, method=method), timeout=35) as response:
            return json.load(response)
    def api(member, values=None, client=1, error=0):
        values_with_id = {'ClientID': client, 'ClientTransactionID': 42, **(values or {})}
        path = '/api/v1/covercalibrator/0/' + member
        response = request(path + ('?' + urllib.parse.urlencode(values_with_id) if values is None else ''),
                           None if values is None else values_with_id, 'GET' if values is None else 'PUT')
        assert response['ErrorNumber'] == error and response['ClientTransactionID'] == 42, response
        return response.get('Value')
    def call(command, **values):
        if command == 'status':
            state = json.loads(api('action', {'Action': 'zwogain.status', 'Parameters': ''}))
            assert api('calibratorstate') == (3 if state['calibrator_on'] else 1)
            assert api('brightness') == (state['brightness'] if state['light_on'] else 0)
            # Multiple readbacks can span the end of a movement; use the atomic
            # native snapshot for motion assertions, not a later HTTP request.
            return state
        member = {'open':'opencover', 'close':'closecover', 'halt':'haltcover', 'on':'calibratoron', 'off':'calibratoroff'}[command]
        return api(member, {'Brightness': values['brightness']} if command == 'on' else {})
    with tempfile.TemporaryDirectory(prefix='zwogain-ofp2-') as temporary:
        profiles = Path(temporary) / 'cameras.json'
        with (Path(temporary)/'server.log').open('w+b') as log:
            process = subprocess.Popen([str(binary('zwogain-alpaca')), *simulate, '--no-discovery',
                                        '--port', str(port), '--profiles', str(profiles)], stdout=log, stderr=log)
            try:
                deadline = time.monotonic()+30
                while True:
                    try:
                        profile = request('/setup/api/flatpanel')['profile']
                        break
                    except urllib.error.URLError:
                        assert process.poll() is None and time.monotonic() < deadline
                        time.sleep(.05)
                assert api('interfaceversion') == 1
                api('brightness', error=0x407)
                assert 'ZwoGain.Status' in api('supportedactions')
                choices = request('/setup/api/flatpanel/discover', {})
                assert any(d['serial'] == serial for d in choices)
                profile['serial'] = serial
                try:
                    request('/setup/api/flatpanel', profile, origin='http://unrelated.invalid')
                    raise AssertionError('Cross-origin profile write allowed')
                except urllib.error.HTTPError as e:
                    assert e.code == 403
                request('/setup/api/flatpanel', profile)
                api('connected', {'Connected': True})
                api('connected', {'Connected': True}, client=2)
                api('connected', {'Connected': False})
                assert api('connected', client=2)
                api('brightness', error=0x407)
                api('connected', {'Connected': True})
                assert api('maxbrightness') == 4096
                for invalid in (-1, 4097, 'x', '1.5'):
                    api('calibratoron', {'Brightness': invalid}, error=0x401)
                api('action', {'Action':'bad','Parameters':''}, error=0x40c)
                api('commandblind', {'Command':'STOP','Raw':False}, error=0x400)
                api('brightness', {'Brightness':128}, error=0x400)
                report['alpaca'] = exercise(call, 'Alpaca CoverCalibrator')
                assert api('coverstate') in (1, 3)
                for client in (1, 2):
                    api('connected', {'Connected': False}, client=client)
                saved = json.loads(profiles.with_suffix('.ofp2.json').read_text(encoding='utf-8'))
                assert saved == profile
                devices = request('/management/v1/configureddevices')['Value']
                assert len(devices) == 1 and devices[0]['DeviceType'] == 'CoverCalibrator', devices
                assert devices[0]['UniqueID'] == profile['uniqueId']
                api('connected', {'Connected': True})
                api('connected', {'Connected': False})
                print('Alpaca discovery, persistent identity, clients, reconnect, validation and origin checks passed.', flush=True)
            finally:
                process.terminate()
                process.wait(timeout=10)
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2)+'\n', encoding='utf-8')


if __name__ == '__main__':
    main()
