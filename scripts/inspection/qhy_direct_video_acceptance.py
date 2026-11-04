"""Explicit physical QHY600 full-frame direct video acceptance; no SDK or image files."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import threading
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    root = Path(__file__).resolve().parents[2]
    log = (args.output/'worker.log').open('w', encoding='utf-8')
    results = (args.output/'results.jsonl').open('w', encoding='utf-8')
    proc = subprocess.Popen([str(root/'target/release/regain-device.exe'), 'qhy', 'camera-direct', '--serve'],
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log,
                            creationflags=subprocess.CREATE_NO_WINDOW)
    timer = threading.Timer(600, proc.kill)
    timer.start()
    sequence = 0
    hashes = set()

    def record(value):
        results.write(json.dumps(value)+'\n')
        results.flush()
        print(json.dumps(value), flush=True)

    def exact(count):
        data = bytearray(count)
        offset = 0
        while offset < count:
            n = proc.stdout.readinto(memoryview(data)[offset:])
            if not n:
                raise RuntimeError('QHY worker exited')
            offset += n
        return data

    def call(method, params=None, reject=False, delay_read=0):
        nonlocal sequence
        sequence += 1
        encoded = json.dumps(dict(version=1,id=sequence,method=method,params=params)).encode()
        proc.stdin.write(struct.pack('<I',len(encoded))+encoded)
        proc.stdin.flush()
        if delay_read:
            time.sleep(delay_read)
        size, = struct.unpack('<I',exact(4))
        assert 0 < size <= 65536
        reply = json.loads(exact(size))
        assert reply['id'] == sequence
        if reject:
            assert not reply['ok'] and reply['binaryLength'] == 0, reply
            return reply
        if not reply['ok']:
            raise RuntimeError(f'{method}: {reply}')
        n = reply['binaryLength']
        assert n in (0, 9600*6422*2), n
        value = reply['result']
        if n:
            data = exact(n)
            digest = hashlib.sha256(data).hexdigest()
            assert digest not in hashes, 'Duplicate frame'
            hashes.add(digest)
            assert (value['width'],value['height']) == (9600,6422)
            value['sha256'] = digest
        return value

    def params(us=100_000, fps=4, gain=30, mode='video'):
        return dict(mode=mode,width=9600,height=6422,bin=1,x=0,y=0,
                    microseconds=us,dark=False,maxFps=fps,gain=gain)

    def capture(p, label):
        start = time.monotonic()
        call('start',p)
        while True:
            state = call('status')
            if state == 2: break
            assert state == 1, state
            assert time.monotonic()-start < 80, 'Capture deadline'
            time.sleep(.01)
        value = call('download')
        assert value['mode'] == p['mode'], value
        record(dict(case=label,seconds=time.monotonic()-start,metadata=value))
        return value

    def stream_frames(count, label, minimum_generation=1):
        received = 0
        start = time.monotonic()
        while received < count:
            value = call('stream-poll')
            if value.get('sha256'):
                assert value['settingsSettled'] and value['settingsGeneration'] >= minimum_generation
                record(dict(case=label,metadata=value))
                received += 1
            else:
                assert not value['continuous']['error'], value
                time.sleep(.02)
            assert time.monotonic()-start < 100, 'Stream deadline'

    try:
        record(dict(case='configuration',workerSha256=hashlib.sha256((root/'target/release/regain-device.exe').read_bytes()).hexdigest(),
                    source=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),
                    sourceDirty=bool(subprocess.check_output(['git','status','--porcelain'],cwd=root,text=True).strip())))
        opened = call('open',dict(name='QHY600M',serial='qhy600m-96763b4ab6a79e117'))
        assert opened['captureModes'] == ['still','video']
        record(dict(case='open',capabilities=opened))
        # Unsupported settings must fail before any acquisition is dispatched.
        for invalid in [dict(dark=True),dict(width=512),dict(microseconds=30_000_001),dict(readRetries=1),dict(maxFps=0)]:
            call('validate', params() | invalid, reject=True)
        for i in range(10): capture(params(),f'repeated-{i}')
        prior = None
        for i in range(3):
            capture(params(fps=.5),f'paced-{i}')
            now = time.monotonic()
            if prior is not None: assert now-prior >= 1.95
            prior = now
        # A paused reader must not pause the independent acquisition owner.
        time.sleep(4)
        paused = capture(params(), 'slow-consumer')
        assert paused['video']['discardedFrames'] > 0
        for i in range(3): capture(params(us=1_000_000,gain=60),f'changed-{i}')
        call('stop')
        call('start',params(us=5_000_000))
        time.sleep(.2)
        begin = time.monotonic()
        call('stop')
        assert time.monotonic()-begin < 3, 'USB pending read did not cancel promptly'
        record(dict(case='cancel-pending-read',seconds=time.monotonic()-begin))
        for i in range(3): capture(params(),f'restarted-{i}')
        call('start',params(fps=.01))
        time.sleep(.2)
        begin=time.monotonic()
        call('stop')
        assert time.monotonic()-begin < 3, 'Delivery pacing delayed cancellation'
        record(dict(case='cancel-fps-wait',seconds=time.monotonic()-begin))
        call('stream-start',params(fps=2))
        stream_frames(10,'stream')
        before = call('stream-status')
        time.sleep(4)
        after = call('stream-status')
        assert after['rawFrames'] > before['rawFrames'] and after['replacedFrames'] > before['replacedFrames']
        # Block the pipe writer on an actual full image, not just between polls.
        before = call('stream-status')
        blocked = call('stream-poll',delay_read=4)
        assert blocked.get('sha256'), blocked
        after = call('stream-status')
        assert after['rawFrames'] > before['rawFrames']+1
        assert after['replacedFrames'] > before['replacedFrames']
        record(dict(case='blocked-output',before=before,after=after,metadata=blocked))
        call('stream-start',params(us=500_000,gain=40,fps=2))
        stream_frames(5,'stream-settings',minimum_generation=2)
        call('stream-stop')
        for i in range(3): capture(params(us=5_000_000,mode='still'),f'still-{i}')
        call('close')
        record(dict(case='complete',distinctFrames=len(hashes)))
    finally:
        try: call('close')
        except Exception: pass
        proc.stdin.close()
        try: proc.wait(timeout=10)
        except subprocess.TimeoutExpired: proc.kill(); proc.wait()
        timer.cancel()
        log.close()
        results.close()


if __name__ == '__main__':
    main()
