"""Exercise the real P25 worker's transfer deadline and error protocol without the SDK."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from importlib import import_module

Worker = import_module('test-rust').Worker
ROOT = Path(__file__).resolve().parents[2]


class ProtocolWorker(Worker):
    def raw(self, method, params=None):
        self.sequence += 1
        request = json.dumps(dict(version=1, id=self.sequence, method=method, params=params or {})).encode()
        self.process.stdin.write(struct.pack('<I', len(request)) + request)
        self.process.stdin.flush()
        size, = struct.unpack('<I', self.read(4))
        assert 0 < size <= 65536
        reply = json.loads(self.read(size))
        assert reply['id'] == self.sequence and reply['version'] == 1
        assert 0 <= reply['binaryLength'] <= 128 * 1024 * 1024
        return reply, self.read(reply['binaryLength'])

    def wait_capture(self):
        until = time.monotonic() + 20
        while True:
            reply, _ = self.raw('status')
            if not reply['ok'] or reply['result'] == 2:
                return reply
            assert reply['result'] == 1 and time.monotonic() < until, reply
            time.sleep(.025)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--worker', type=Path, default=ROOT / 'target/release/zwogain-direct.exe')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    with args.output.open('x', encoding='utf-8') as output:
        with ProtocolWorker([str(args.worker), '--serve']) as worker:
            worker.call('open', dict(name='ZWO ASI2600MM Pro'))
            try:
                worker.call('set', dict(control=0, value=100))
                worker.call('set', dict(control=5, value=50))
                exposure = dict(width=6248, height=4176, x=0, y=0, bin=1,
                                microseconds=100000, dark=True, readRetries=0)
                rejected, pixels = worker.raw('start', dict(exposure, transferTimeoutSeconds=-1))
                assert not rejected['ok'] and rejected['sdkCode'] == 8 and not pixels, rejected
                worker.call('start', dict(exposure, transferTimeoutSeconds=.005))
                failed = worker.wait_capture()
                assert not failed['ok'] and failed['sdkCode'] is None, failed
                detail = failed['transportFailure']
                assert detail['deadlineExpired'] and detail['category'] in ['timeout', 'budget_exhausted'], failed
                assert detail['frameBytes'] == 52183296, failed
                download, pixels = worker.raw('download')
                assert not download['ok'] and download['transportFailure'] == detail and not pixels, download
                # A fresh, explicitly requested exposure still works after cleanup.
                worker.call('start', dict(exposure, transferTimeoutSeconds=60))
                assert worker.wait_capture()['ok']
                metadata, pixels = worker.call('download')
                assert len(pixels) == 52183296 and not metadata['sdkLoaded']
                assert hashlib.sha256(pixels).hexdigest() == metadata['sha256']
                output.write(json.dumps(dict(workerSha256=hashlib.sha256(args.worker.read_bytes()).hexdigest(),
                    invalidTimeoutRejected=True, failure=detail, errorDownloadBytes=0,
                    subsequentCapture=metadata), indent=2) + '\n')
            finally:
                worker.call('close')
    print('Transfer deadline, structured status/download errors, and subsequent full frame passed.')


if __name__ == '__main__':
    main()
