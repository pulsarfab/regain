"""Exercise native camera IPC without starting an HTTP server; prepare COM fixtures."""
import argparse
import json
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import time


class Camera:
    def __init__(self, binary, profile):
        self.process = subprocess.Popen([str(binary), '--simulate', '--profiles', str(profile)],
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                        creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
        self.id = 0

    def call(self, method, member='', params=None, error=0):
        self.id += 1
        request = json.dumps(dict(version=1, id=self.id, method=method, member=member, params=params)).encode()
        self.process.stdin.write(struct.pack('<I', len(request)) + request)
        self.process.stdin.flush()
        header = self.process.stdout.read(4)
        assert len(header) == 4, 'Native worker exited'
        reply = json.loads(self.process.stdout.read(struct.unpack('<I', header)[0]))
        assert reply['version'] == 1 and reply['id'] == self.id, reply
        assert reply['errorNumber'] == error, reply
        pixels = self.process.stdout.read(reply['binaryLength'])
        return (reply.get('value'), pixels) if reply['binaryLength'] else reply.get('value')

    def close(self):
        self.process.stdin.close()
        try:
            assert self.process.wait(timeout=15) == 0
        finally:
            if self.process.poll() is None:
                self.process.kill()
            self.process.stdout.close()


def exercise(binary, directory, prepare=False):
    for slot in range(4):
        path = directory / f'camera-{slot + 1}.json'
        camera = Camera(binary, path)
        try:
            direct = slot != 0
            profile = camera.call('profile')
            choices = camera.call('discover', params={'direct': direct})
            profile.update(camera=choices[slot if direct else 0], serial=None, direct=direct)
            profile['recovery']['reconnectDelaySeconds'] = .05
            camera.call('configure', params=profile)
            if prepare:
                continue
            camera.call('get', 'imageready', error=0x407)
            # A separate setup instance saves after this driver has been created.
            other = Camera(binary, path)
            try:
                changed = other.call('profile')
                changed['controls']['0'] = 101
                other.call('configure', params=changed)
            finally:
                other.close()
            camera.call('put', 'connected', {'Connected': True})
            assert camera.call('get', 'gain') == 101
            camera.call('configure', params=profile, error=0x40B)
            camera.call('put', 'numx', {'NumX': 64})
            camera.call('put', 'numy', {'NumY': 64})
            camera.call('put', 'startexposure', {'Duration': .01, 'Light': False})
            deadline = time.monotonic() + 20
            while not camera.call('get', 'imageready'):
                assert time.monotonic() < deadline
                time.sleep(.02)
            dimensions, pixels = camera.call('get', 'imagearray')
            assert dimensions == {'width': 64, 'height': 64} and len(pixels) == 8192
            camera.call('put', 'startexposure', {'Duration': 5, 'Light': False})
            camera.call('put', 'abortexposure')
            assert not camera.call('get', 'imageready')
            assert camera.call('get', 'camerastate') == 0
            camera.call('put', 'disconnect')
            while camera.call('get', 'connecting'):
                time.sleep(.02)
            assert not camera.call('get', 'connected')
        finally:
            camera.close()
    print('Native camera: four SDK/direct slots, selection reload, capture, binary frame, abort and pipe cleanup passed.' if not prepare else 'Prepared four native COM profiles.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin-dir', default='target/debug')
    parser.add_argument('--prepare', type=Path)
    args = parser.parse_args()
    binary = Path(args.bin_dir).resolve() / ('zwogain-camera.exe' if os.name == 'nt' else 'zwogain-camera')
    if args.prepare:
        args.prepare.mkdir(parents=True, exist_ok=True)
        exercise(binary, args.prepare.resolve(), True)
    else:
        with tempfile.TemporaryDirectory(prefix='zwogain-native-') as directory:
            exercise(binary, Path(directory))
