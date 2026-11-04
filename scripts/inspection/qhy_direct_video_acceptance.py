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
    parser.add_argument('--bits', type=int, choices=[8,16], default=16)
    parser.add_argument('--read-mode', type=int, choices=[0,1,2,3,8,9], default=0)
    parser.add_argument('--all-read-modes', action='store_true', help='Full-sensor mode/format/gain/cancellation and stream-switch matrix')
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    root = Path(__file__).resolve().parents[2]
    log = (args.output/'worker.log').open('w', encoding='utf-8')
    results = (args.output/'results.jsonl').open('w', encoding='utf-8')
    proc = subprocess.Popen([str(root/'target/release/regain-device.exe'), 'qhy', 'camera-direct', '--serve'],
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log,
                            creationflags=subprocess.CREATE_NO_WINDOW)
    timer = threading.Timer(1800 if args.all_read_modes else 600, proc.kill)
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
        assert n in (0, 9600*6422, 9600*6422*2), n
        value = reply['result']
        if n:
            assert value['bitDepth'] in (8,16) and n == 9600*6422*(value['bitDepth']//8), value
            data = exact(n)
            digest = hashlib.sha256(data).hexdigest()
            assert digest not in hashes, 'Duplicate frame'
            hashes.add(digest)
            assert (value['width'],value['height']) == (9600,6422)
            value['sha256'] = digest
        return value

    def params(us=100_000, fps=4, gain=30, mode='video', bits=None, read_mode=None):
        return dict(mode=mode,width=9600,height=6422,bin=1,x=0,y=0,
                    microseconds=us,dark=False,maxFps=fps,gain=gain,
                    bitDepth=(16 if mode=='still' else args.bits) if bits is None else bits,
                    readMode=(0 if mode=='still' else args.read_mode) if read_mode is None else read_mode)

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
        assert value['bitDepth'] == p['bitDepth'], value
        assert value['hardwareReadMode'] == p['readMode'], value
        record(dict(case=label,seconds=time.monotonic()-start,metadata=value))
        return value

    def stream_frames(count, label, minimum_generation=1, bits=None, read_mode=None):
        received = 0
        start = time.monotonic()
        while received < count:
            value = call('stream-poll')
            if value.get('sha256'):
                assert value['settingsSettled'] and value['settingsGeneration'] >= minimum_generation
                assert value['bitDepth'] == (args.bits if bits is None else bits), value
                assert value['hardwareReadMode'] == (args.read_mode if read_mode is None else read_mode), value
                record(dict(case=label,metadata=value))
                received += 1
            else:
                assert not value['continuous']['error'], value
                time.sleep(.02)
            assert time.monotonic()-start < 100, 'Stream deadline'

    try:
        record(dict(case='configuration',bitDepth=args.bits,readMode=args.read_mode,allReadModes=args.all_read_modes,workerSha256=hashlib.sha256((root/'target/release/regain-device.exe').read_bytes()).hexdigest(),
                    source=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),
                    sourceDirty=bool(subprocess.check_output(['git','status','--porcelain'],cwd=root,text=True).strip())))
        opened = call('open',dict(name='QHY600M',serial='qhy600m-96763b4ab6a79e117'))
        assert opened['captureModes'] == ['still','video']
        assert opened['videoBitDepths'] == [8,16]
        assert [m['id'] for m in opened['videoReadModes']] == [0,1,2,3,8,9]
        record(dict(case='open',capabilities=opened))
        # Unsupported settings must fail before any acquisition is dispatched.
        for invalid in [dict(dark=True),dict(width=512),dict(microseconds=30_000_001),dict(readRetries=1),dict(maxFps=0),dict(bitDepth=12),dict(bitDepth='8'),dict(mode='still',bitDepth=8)]:
            call('validate', params() | invalid, reject=True)
        for invalid in [dict(readMode=m) for m in [4,5,6,7,10,-1,256,'1',1.5]] + [dict(mode='still',bitDepth=16,readMode=1)]:
            call('validate',params() | invalid,reject=True)
        if args.all_read_modes:
            for read_mode in [0,1,2,3,8,9]:
                for bits in [8,16]:
                    for i in range(5):
                        capture(params(read_mode=read_mode,bits=bits),f'mode-{read_mode}-raw{bits}-{i}')
                # Capture gain extremes and representative conversion/digital boundaries.
                for gain in [0,25,26,55,56,100,101,200]:
                    capture(params(read_mode=read_mode,bits=16,gain=gain),f'mode-{read_mode}-gain-{gain}')
                call('stop')
                call('start',params(read_mode=read_mode,bits=8,us=5_000_000))
                time.sleep(.2)
                begin=time.monotonic();call('stop')
                assert time.monotonic()-begin < 3
                capture(params(read_mode=read_mode,bits=8),f'mode-{read_mode}-after-cancel')
                capture(params(mode='still',us=5_000_000),f'mode-{read_mode}-return-still')
            generation=0
            # One continuous connection, crossing every mode in both directions.
            for read_mode in [9,8,3,2,1,0]:
                for bits in [16,8]:
                    generation+=1
                    call('stream-start',params(read_mode=read_mode,bits=bits,fps=2))
                    stream_frames(3,f'stream-mode-{read_mode}-raw{bits}',generation,bits,read_mode)
                    before=call('stream-status')
                    deadline=time.monotonic()+10
                    while not before['ready']:
                        assert time.monotonic()<deadline and not before['error']
                        time.sleep(.02);before=call('stream-status')
                    blocked=call('stream-poll',delay_read=4)
                    after=call('stream-status')
                    assert blocked.get('sha256') and blocked['hardwareReadMode']==read_mode and blocked['bitDepth']==bits
                    assert after['rawFrames']>before['rawFrames'] and after['replacedFrames']>before['replacedFrames']
                    record(dict(case=f'blocked-mode-{read_mode}-raw{bits}',metadata=blocked,before=before,after=after))
            call('stream-stop')
            for i in range(3):capture(params(mode='still',us=5_000_000),f'final-still-{i}')
            call('close');record(dict(case='complete',distinctFrames=len(hashes)))
            return
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
        for bits in (24-args.bits, args.bits):
            for i in range(3): capture(params(bits=bits),f'format-{bits}-{i}')
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
        for generation,bits in enumerate((24-args.bits,args.bits),start=3):
            call('stream-start',params(us=500_000,gain=40,fps=2,bits=bits))
            stream_frames(3,f'stream-format-{bits}',minimum_generation=generation,bits=bits)
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
