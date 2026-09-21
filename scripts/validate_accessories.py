"""Validate the native EFW/EAF worker; real motion requires --hardware --exercise."""
import argparse
import json
from pathlib import Path
import subprocess
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--worker', type=Path, default=Path('target/debug/regain-device.exe'))
p.add_argument('--hardware', action='store_true')
p.add_argument('--exercise', action='store_true')
p.add_argument('--calibrate', action='store_true', help='also calibrate the EFW; runs automatically in simulation')
p.add_argument('--device', choices=['efw', 'eaf'])
args = p.parse_args()
for kind in ('efw', 'eaf'):
    if args.device and args.device != kind:
        continue
    command = [str(args.worker.resolve()), "zwo", kind, 'serve']
    if not args.hardware:
        command.append('--simulate')
    worker = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    def request(command, **values):
        worker.stdin.write(json.dumps(dict(command=command, **values))+'\n')
        worker.stdin.flush()
        reply = json.loads(worker.stdout.readline())
        assert reply['ok'], reply
        return reply['result']
    def wait(position):
        deadline = time.monotonic()+30
        while time.monotonic() < deadline:
            status = request('status')
            assert not status['error'] and not status.get('fault'), status
            if not status['moving']:
                assert status['position'] == position, status
                return status
            time.sleep(.1)
        raise TimeoutError('motion timeout')
    try:
        identity = request('identity')
        initial = request('status')
        print(kind, identity, initial)
        if kind == 'efw' and (args.calibrate or not args.hardware):
            started = time.monotonic()
            request('calibrate')
            active = request('status')
            assert active['calibrating'] and active['moving'] and active['position'] == -1 and active['slots'] == initial['slots'], active
            for command, values in [('calibrate', {}), ('move', {'position': 1}), ('halt', {})]:
                worker.stdin.write(json.dumps(dict(command=command, **values))+'\n'); worker.stdin.flush()
                assert not json.loads(worker.stdout.readline())['ok']
            deadline = time.monotonic()+95
            while True:
                status = request('status')
                assert not status['error'] and not status.get('fault') and status['slots'] == initial['slots'], status
                if not status['calibrating']:
                    assert not status['moving'] and status['position'] == 0, status
                    break
                assert time.monotonic() < deadline
                time.sleep(.1)
            print('EFW calibration passed in', round(time.monotonic()-started, 3), 'seconds; detected', status['slots'], 'slots')
            request('move', position=initial['position']); wait(initial['position'])
        if args.exercise or not args.hardware:
            assert not initial['moving']
            start = initial['position']
            targets = list(range(initial['slots']))+[start] if kind == 'efw' else [start+50 if start+50 <= initial['max_step'] else start-50, start]
            for target in targets:
                request('move', position=target)
                wait(target)
            if kind == 'eaf':
                target = start + 50 if start + 50 <= initial['max_step'] else start - 50
                request('move', position=target)
                request('halt')
                stopped = request('status')
                assert not stopped['moving'] and min(start, target) <= stopped['position'] <= max(start, target), stopped
                print('EAF halt confirmed at', stopped['position'])
                request('move', position=start)
                wait(start)
                try:
                    request('settings', beep=not initial['beep'])
                    assert request('status')['beep'] != initial['beep']
                finally:
                    request('settings', beep=initial['beep'])
            final = request('status')
            for field in ('position', 'max_step', 'backlash', 'beep', 'reverse'):
                assert final[field] == initial[field], (field, initial, final)
            print(kind, 'motion and restoration passed')
    finally:
        worker.stdin.close()
        try:
            assert worker.wait(timeout=5) == 0
        except subprocess.TimeoutExpired:
            worker.kill()
            raise
