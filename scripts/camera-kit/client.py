"""Owned SDK host, bounded binary protocol; never attach to another application."""
import hashlib
import json
import struct
import subprocess
import threading
import time


class HostError(RuntimeError):
    pass


class Host:
    def __init__(self, executable, sdk, simulate=False):
        args = [str(executable), '--sdk', str(sdk)]
        if simulate:
            args.append('--simulate')
        self.proc = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.DEVNULL,
                                     creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
        self.next_id = 0
        self.expired = False
        self.record = lambda value: None

    def kill(self):
        if self.proc.poll() is None:
            self.proc.kill()

    def _exact(self, size):
        data = bytearray()
        while len(data) < size:
            chunk = self.proc.stdout.read(size - len(data))
            if not chunk:
                raise HostError('SDK host deadline expired' if self.expired else 'SDK host exited')
            data.extend(chunk)
        return data

    def call(self, method, params=None, timeout=20, sample=None):
        if timeout <= 0:
            raise HostError('Exercise deadline expired')
        self.next_id += 1
        request = dict(version=1, id=self.next_id, method=method, params=params)
        # Camera serial is used for identity checks, never recorded in commands.
        logged = {k: v for k, v in (params or {}).items() if k != 'serial'}
        self.record(dict(kind='command', name=method, parameters=logged))
        started = time.monotonic()

        def expire():
            self.expired = True
            self.kill()

        timer = threading.Timer(timeout, expire)
        timer.start()
        try:
            data = json.dumps(request).encode()
            self.proc.stdin.write(struct.pack('<I', len(data)) + data)
            self.proc.stdin.flush()
            length, = struct.unpack('<I', self._exact(4))
            if not 0 < length <= 65536:
                self.kill()
                raise HostError('Invalid protocol JSON length')
            reply = json.loads(self._exact(length))
            if reply.get('id') != self.next_id or reply.get('version') != 1:
                self.kill()
                raise HostError('Protocol response does not match request')
            size = reply.get('binaryLength')
            if type(size) is not int or not 0 <= size <= 512 * 1024 * 1024:
                self.kill()
                raise HostError('Invalid protocol binary length')
            if size and method != 'download':
                self.kill()
                raise HostError('Unexpected binary response')
            digest = hashlib.sha256()
            remaining = size
            output = sample.open('xb') if sample and size else None
            try:
                while remaining:
                    chunk = self._exact(min(remaining, 1024 * 1024))
                    digest.update(chunk)
                    if output:
                        output.write(chunk)
                    remaining -= len(chunk)
            finally:
                if output:
                    output.close()
            self.record(dict(kind='reply', name=method, ok=reply.get('ok'),
                             elapsedMs=round((time.monotonic() - started) * 1000), bytes=size,
                             error=reply.get('error')))
            if not reply.get('ok'):
                raise HostError(str(reply.get('error', 'SDK command failed')))
            return reply['result'], dict(bytes=size, sha256=digest.hexdigest())
        finally:
            timer.cancel()
            timer.join()

    def dispose(self):
        self.kill()
        self.proc.wait(timeout=5)
        self.proc.stdin.close()
        self.proc.stdout.close()
