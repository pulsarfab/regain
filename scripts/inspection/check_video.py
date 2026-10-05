"""Explicit manual ASI585MM Pro/ASI662MC/ASI676MC video validation; no image files or SDK.

Run only with an idle operator-authorized selected model. Other models are not opened.
Use --simulate for protocol development without hardware. Results are local JSONL.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import threading
import time

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--simulate', action='store_true')
    parser.add_argument('--worker', type=Path, default=ROOT / 'target/debug/regain-device.exe')
    parser.add_argument('--long-transition-only', action='store_true',
                        help='Reproduce gain-300 6.4s -> 25s changes and repeated 25s frames')
    parser.add_argument('--camera-name', choices=['ZWO ASI662MC', 'ZWO ASI676MC', 'ZWO ASI585MM Pro'], default='ZWO ASI662MC')
    args = parser.parse_args()
    full_width, full_height = {'ZWO ASI662MC': (1920, 1080), 'ZWO ASI676MC': (3552, 3552), 'ZWO ASI585MM Pro': (3840, 2160)}[args.camera_name]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    command = [str(args.worker.resolve()), 'zwo', 'camera-direct', '--serve']
    if args.simulate:
        command.append('--simulate')
    with args.output.open('x', encoding='utf-8') as log:
        proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        timer = threading.Timer(300 if args.long_transition_only else 180, proc.kill)
        timer.start()
        diagnostics = []

        def drain():
            for line in proc.stderr:
                diagnostics.append(line.decode(errors='replace').rstrip())

        reader = threading.Thread(target=drain)
        reader.start()

        def record(value):
            log.write(json.dumps(value) + '\n')
            log.flush()

        def exact(length):
            data = bytearray()
            while len(data) < length:
                part = proc.stdout.read(length - len(data))
                if not part:
                    raise RuntimeError('worker closed its response pipe')
                data.extend(part)
            return data

        request_id = 0

        def call(method, params=None):
            nonlocal request_id
            request_id += 1
            data = json.dumps(dict(version=1, id=request_id, method=method, params=params)).encode()
            proc.stdin.write(struct.pack('<I', len(data)) + data)
            proc.stdin.flush()
            length, = struct.unpack('<I', exact(4))
            if not 0 < length <= 65536:
                raise RuntimeError('invalid protocol response')
            reply = json.loads(exact(length))
            if reply['id'] != request_id or not reply['ok']:
                raise RuntimeError(str(reply))
            size = reply.get('binaryLength', 0)
            if not 0 <= size <= full_width * full_height * 2:
                raise RuntimeError('invalid pixel byte count')
            pixels = exact(size)
            if size:
                result = reply['result']
                if size != result['width'] * result['height'] * 2:
                    raise RuntimeError('truncated frame')
                digest = hashlib.sha256(pixels).hexdigest()
                if 'sha256' in result and result['sha256'] != digest:
                    raise RuntimeError('frame checksum mismatch')
                # Aggregate local diagnostics only; no images or sensor samples retained.
                sample = sorted(struct.unpack_from('<H', pixels, offset)[0]
                                for offset in range(0, len(pixels), 128))
                result['diagnosticRaw16P50'] = sample[len(sample) // 2]
                result['diagnosticRaw16P90'] = sample[len(sample) * 9 // 10]
            return reply['result']

        def params(us=100000, width=full_width, height=full_height, fps=2.0):
            return dict(mode='video', maxFps=fps, width=width, height=height,
                        x=0, y=0, bin=1, microseconds=us, dark=False)

        def capture(configuration):
            begin = time.monotonic()
            call('start', configuration)
            while call('status') == 1:
                time.sleep(.01)
            result = call('download')
            if result['mode'] != configuration['mode']:
                raise RuntimeError('wrong capture mode')
            record(dict(kind='frame', configuration=configuration, metadata=result,
                        requestMs=(time.monotonic() - begin) * 1000,
                        receivedAt=time.monotonic()))
            return result

        try:
            record(dict(kind='configuration', simulate=args.simulate, cameraName=args.camera_name,
                        workerSha256=hashlib.sha256(Path(command[0]).read_bytes()).hexdigest(),
                        sourceDirty=bool(subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT, text=True).strip()),
                        source=subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()))
            opened = call('open', dict(name=args.camera_name))
            if 'video' not in opened['captureModes']:
                raise RuntimeError('video capability absent')
            if args.long_transition_only:
                call('set', dict(control=0, value=300))
                for us in [6400000, 25000000, 25000000, 6400000, 25000000]:
                    capture(params(us=us, fps=.5))
                call('close')
                proc.stdin.close()
                if proc.wait(timeout=10) != 0:
                    raise RuntimeError('worker failed on exit')
                record(dict(kind='experiment-complete'))
                return
            # No discovery calls; exact model selection refuses ambiguous matches.
            for _ in range(6):
                capture(params())
            # A lower rate must cap reads, not merely output/display updates.
            previous = None
            for _ in range(3):
                capture(params(fps=.5))
                now = time.monotonic()
                if previous is not None and now - previous < 1.95:
                    raise RuntimeError('fractional FPS limit was exceeded')
                previous = now
            call('stop')
            for _ in range(4):
                capture(params(width=512, height=256, us=1000, fps=10))
            # Exercise a slow consumer without discovery/reinitialization.
            time.sleep(1)
            capture(params(width=512, height=256, us=1000, fps=10))
            call('set', dict(control=0, value=300))
            for us in [400000, 1600000, 6400000, 6400000, 30000000]:
                capture(params(us=us))
            # Cancellation while the long-exposure low-power gate is active.
            call('start', params(us=30000000))
            time.sleep(1.3)
            start = time.monotonic()
            call('stop')
            stopped_ms = (time.monotonic() - start) * 1000
            if stopped_ms > 8000 or call('status') != 0:
                raise RuntimeError('video stop did not return promptly to idle')
            record(dict(kind='cancelled-long-exposure', stopMs=stopped_ms))
            capture(params())
            call('stop')
            capture(params(fps=.01))
            call('start', params(fps=.01))
            time.sleep(.1)
            start = time.monotonic()
            call('stop')
            stopped_ms = (time.monotonic() - start) * 1000
            if stopped_ms > 8000:
                raise RuntimeError('FPS wait was not cancellable')
            record(dict(kind='cancelled-fps-wait', stopMs=stopped_ms))
            configuration = params(width=512, height=256)
            configuration['mode'] = 'still'
            # Still metadata predates the explicit mode field.
            call('start', configuration)
            while call('status') == 1:
                time.sleep(.01)
            result = call('download')
            record(dict(kind='still-regression', bytes=result.get('bytes'), width=result['width'], height=result['height']))
            call('close')
            proc.stdin.close()
            if proc.wait(timeout=10) != 0:
                raise RuntimeError('worker failed on exit')
            record(dict(kind='experiment-complete'))
        except Exception:
            record(dict(kind='diagnostics', lines=diagnostics[-20:]))
            raise
        finally:
            if proc.poll() is None:
                try:
                    call('close')
                    proc.stdin.close()
                    proc.wait(timeout=10)
                except Exception:
                    proc.kill()
            proc.wait(timeout=5)
            timer.cancel()
            timer.join()
            reader.join(timeout=5)
    print(args.output)


if __name__ == '__main__':
    main()
