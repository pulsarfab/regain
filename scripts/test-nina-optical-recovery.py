"""Exercise the test-only installed-NINA optical harness; never resets real USB.

Build tests/fixtures/nina-optical-acceptance and optical-worker, load the harness
in an isolated NINA profile, and pass its explicit acceptance directory. Accessory
workers must be OpticalWorkerFixture; camera captures use the selected camera.
"""
import argparse
import json
import time
from pathlib import Path


def atomic_json(path, value):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value), encoding='utf-8')
    for attempt in range(50):
        try:
            temporary.replace(path)
            return
        except PermissionError:
            if attempt == 49:
                raise
            time.sleep(.02)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--width', type=int, required=True)
    parser.add_argument('--height', type=int, required=True)
    args = parser.parse_args()
    root = args.directory.resolve()
    results = []

    def request(identifier, command, inject=None, succeeds=True):
        response = root / (identifier + '.json')
        if response.exists():
            raise RuntimeError('Use a fresh acceptance directory; results must not be overwritten')
        atomic_json(root / 'request.json', {'id': identifier, 'command': command})
        if inject:
            time.sleep(1.5)
            inject()
        deadline = time.monotonic() + 120
        while not response.exists():
            if (root / 'harness-error.txt').exists():
                raise RuntimeError((root / 'harness-error.txt').read_text())
            if time.monotonic() > deadline:
                raise TimeoutError(identifier)
            time.sleep(.2)
        result = json.loads(response.read_text())
        results.append(result)
        assert result['ok'] == succeeds, result
        if command == 'capture':
            if succeeds:
                assert (result['width'], result['height']) == (args.width, args.height), result
                assert Path(result['saved']).exists(), result
            else:
                assert not (root / (identifier + '.fits')).exists(), result
        print(identifier, 'PASS', flush=True)
        return result

    def events(kind):
        return [json.loads(line) for file in (root / 'optical-state').glob(kind + '-*.jsonl')
                for line in file.read_text().splitlines()]

    def group_loss():
        for kind in ('efw', 'caa'):
            (root / 'optical-state' / (kind + '-fail-once')).touch()

    def change(kind, **values):
        path = root / 'optical-state' / (kind + '-state.json')
        state = json.loads(path.read_text()) if path.exists() else {
            'position': 3, 'slots': 7, 'moving': False, 'calibrating': False, 'error': 0}
        state.update(values)
        atomic_json(path, state)

    deadline = time.monotonic() + 60
    while not (root / 'ready').exists():
        if time.monotonic() > deadline:
            raise TimeoutError('Installed NINA harness did not initialize')
        time.sleep(.25)
    try:
        request('connect', 'connect')
        request('baseline', 'capture')
        identities_before = {kind: sum(e['command'] == 'identity' for e in events(kind)) for kind in ('efw', 'caa')}
        group_loss()
        request('group-loss', 'capture')
        for kind in ('efw', 'caa'):
            assert sum(e['command'] == 'identity' for e in events(kind)) > identities_before[kind], kind
        request('steady-after-recovery', 'capture')
        interrupted = request('disconnect-during-exposure', 'capture', inject=group_loss, succeeds=False)
        assert 'overlapped' in interrupted['error'].lower()
        request('next-frame-after-interruption', 'capture')
        request('changed-wheel', 'capture', inject=lambda: change('efw', position=0), succeeds=False)
        blocked = request('blocked-follow-up', 'capture', succeeds=False)
        assert 'blocked' in blocked['error'].lower()
        change('efw', position=3)
        request('explicit-camera-reconnect', 'reconnect-camera')

        def changed_caa():
            change('caa', mechanical_degrees=40., logical_degrees=40.)
            (root / 'optical-state/caa-fail-once').touch()

        request('changed-caa', 'capture', inject=changed_caa, succeeds=False)
        request('explicit-disconnect', 'disconnect')
        change('caa', mechanical_degrees=30., logical_degrees=30.)
        request('explicit-reconnect', 'connect')
        request('verified-after-explicit-reconnect', 'capture')
        request('disconnect-wheel', 'disconnect-wheel')
        missing = request('disconnected-wheel-blocked', 'capture', succeeds=False)
        assert 'previously connected' in missing['error'].lower()
        for kind in ('efw', 'caa'):
            forbidden = [e for e in events(kind) if e['command'] in {
                'move', 'move-to', 'move-relative', 'move-mechanical', 'rotate-unwrapped', 'reference', 'reset-origin'}]
            assert not forbidden, forbidden
    finally:
        try:
            request('cleanup-disconnect', 'disconnect')
        finally:
            atomic_json(root / 'acceptance-results.json', {
                'accessories': 'injected worker fixtures, not physical USB disconnects',
                'camera_dimensions': [args.width, args.height], 'results': results})


if __name__ == '__main__':
    main()
