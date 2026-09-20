"""Bounded Duo RAW16 acquisition matrix; retain metadata and statistics, never pixels."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import threading

import numpy as np

ROOT = Path(__file__).resolve().parents[2]


def capture(options, guide=False, asi6200=False, worker=None, asi2600_p25=False):
    command = [str(worker or ROOT / 'target/debug/regain-direct.exe'), '--capture-2600-p25' if asi2600_p25 else '--capture-6200' if asi6200 else '--capture-guide' if guide else '--capture-duo', '--stream']
    for key, value in options.items():
        command += ['--' + key] + ([] if value is True else [str(value)])
    proc = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    # Lifecycle diagnostics can exceed the Windows pipe buffer before stdout.
    # Drain concurrently so logging never stalls a hardware capture.
    errors = bytearray()
    def drain_errors():
        for part in iter(lambda: proc.stderr.read(4096), b''):
            errors.extend(part)
    stderr_thread = threading.Thread(target=drain_errors, daemon=True)
    stderr_thread.start()
    watchdog = threading.Timer((options.get('microseconds', 100000) / 1e6 + 35)
                               * options.get('frames', 1) + 10, proc.kill)
    watchdog.start()

    def exact(length):
        data = bytearray()
        while len(data) < length:
            part = proc.stdout.read(length - len(data))
            if not part:
                proc.wait(timeout=5)
                stderr_thread.join(timeout=5)
                raise RuntimeError('truncated direct stream: ' + errors.decode(errors='replace'))
            data.extend(part)
        return data

    results = []
    try:
        for _ in range(options.get('frames', 1)):
            size = struct.unpack('<I', exact(4))[0]
            if not 0 < size <= 65536:
                raise RuntimeError('invalid metadata size')
            metadata = json.loads(exact(size))
            info = metadata['capture']
            if metadata['sdkLoaded'] or info['sdkLoaded']:
                raise RuntimeError('SDK loaded in direct worker')
            width, height = options.get('width', 9576 if asi6200 else 1920 if guide else 6248), options.get('height', 6388 if asi6200 else 1080 if guide else 4176)
            expected = width * height * 2
            if info['bytes'] != expected or expected > 128 * 1024 * 1024:
                raise RuntimeError('wrong output size')
            data = exact(expected)
            if hashlib.sha256(data).hexdigest() != info['sha256']:
                raise RuntimeError('IPC digest mismatch')
            if options.get('replay') or options.get('replay-prefix-bytes'):
                if not info['replay']['pixelBytesIdentical']:
                    raise RuntimeError('retained pixel mismatch')
            if options.get('interrupt-read-after-bytes'):
                if not info['interruptedPrefixPixelsMatch'] or not info['readRecoveries']:
                    raise RuntimeError('injected read recovery failed')
            if options.get('timeout-read-after-bytes'):
                if (info.get('timeoutInjectionBytes') != options['timeout-read-after-bytes']
                        or not info['interruptedPrefixPixelsMatch'] or not info['readRecoveries']):
                    raise RuntimeError('real USB timeout did not recover the interrupted frame')
            pixels = np.frombuffer(data, dtype='<u2')
            info['statistics'] = {'mean': float(pixels.mean()), 'median': float(np.median(pixels)),
                                  'minimum': int(pixels.min()), 'maximum': int(pixels.max()),
                                  'stddev': float(pixels.std()), 'pixels': len(pixels)}
            # A valid envelope and digest can still describe partially stale
            # sensor memory. Retain vertical profiles for control transitions.
            rows = pixels.reshape(height, width)
            row_medians = np.median(rows, axis=1)
            info['statistics']['rowMedianRange'] = [float(row_medians.min()), float(row_medians.max())]
            info['statistics']['rowBandMedians'] = [float(np.median(band))
                                                    for band in np.array_split(rows, min(16, height))]
            info['statistics']['rowBandMeans'] = [float(band.mean())
                                                  for band in np.array_split(rows, min(16, height))]
            # Deliberately omit USB device identity and paths from this result.
            results.append(info)
        if proc.stdout.read(1) or proc.wait(timeout=5) != 0:
            raise RuntimeError('unexpected stream termination')
        if len({r['sha256'] for r in results}) != len(results):
            raise RuntimeError('repeated digest across fresh exposures')
    finally:
        if proc.poll() is None:
            proc.kill()
        proc.wait(timeout=5)
        stderr_thread.join(timeout=5)
        watchdog.cancel()
        watchdog.join()
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--long', action='store_true', help='include a 30-second full frame')
    args = parser.parse_args()
    cases = [
        dict(width=64, height=64, microseconds=32, gain=-25, offset=0),
        dict(width=512, height=256, x=16, y=32, microseconds=100000, gain=99, offset=50, replay=True),
        dict(width=512, height=256, microseconds=999999, gain=100, offset=50),
        dict(width=512, height=256, microseconds=1000000, gain=460, offset=120),
        dict(width=512, height=256, microseconds=2000000, gain=461, offset=120),
        dict(width=512, height=256, microseconds=100000, gain=700, offset=240),
        dict(microseconds=100000, gain=100, offset=50, frames=3, replay=True),
        dict(width=3120, height=2088, bin=2, microseconds=100000, gain=100, offset=50, frames=2, replay=True),
        dict(width=2080, height=1392, bin=3, microseconds=100000, gain=350, offset=120, frames=2, replay=True),
        dict(width=1560, height=1044, bin=4, microseconds=100000, gain=700, offset=240, frames=2, replay=True),
        dict(width=512, height=256, x=16, y=32, bin=2, microseconds=100000, gain=0, offset=0, replay=True),
        dict(microseconds=100000, gain=100, offset=50, replay=True,
             **{'interrupt-read-after-bytes': 12 * 1024 * 1024}),
    ]
    if args.long:
        cases.append(dict(microseconds=30000000, gain=100, offset=50, replay=True))
    with args.output.open('x', encoding='utf-8') as output:
        for i, options in enumerate(cases):
            results = capture(options)
            output.write(json.dumps({'case': i, 'options': options, 'captures': results}) + '\n')
            output.flush()
            print(f'case {i}: {len(results)} frames, recoveries {[r["readRecoveries"] for r in results]}', flush=True)


if __name__ == '__main__':
    main()
