"""Exercise SDK-free framed capture; keep pixels in memory and record statistics only."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import threading

import numpy as np

ROOT = Path(__file__).resolve().parents[2]


def capture(options):
    command = [str(ROOT / 'target/debug/zwogain-direct.exe'), '--capture', '--stream']
    for key, value in options.items():
        if value is True:
            command += ['--' + key]
            continue
        command += ['--' + key, str(value)]
    proc = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    watchdog = threading.Timer((options.get('microseconds', 100000) / 1e6 + 20) * options.get('frames', 1), proc.kill)
    watchdog.start()
    def exact(length):
        data = bytearray()
        while len(data) < length:
            part = proc.stdout.read(length - len(data))
            if not part:
                raise RuntimeError('truncated direct stream: ' + proc.stderr.read().decode(errors='replace'))
            data.extend(part)
        return bytes(data)
    results = []
    try:
        for _ in range(options.get('frames', 1)):
            size = struct.unpack('<I', exact(4))[0]
            if not 0 < size <= 65536:
                raise RuntimeError('invalid metadata size')
            metadata = json.loads(exact(size))
            info = metadata['capture']
            if (metadata['sdkLoaded'] or info['sdkLoaded'] or not info['sensorFrozenBeforeRead']
                    or info['retentionStatusBeforeArm'] != 1 or not info['transportPixelsReplaced']
                    or not info['defectCorrectionApplied'] or len(info['defectIndexSha256']) != 64):
                raise RuntimeError('incorrect capture lifecycle metadata')
            expected_retries = 1 if options.get('interrupt-read-after-bytes', 0) else 0
            if info['readoutRetriesUsed'] != expected_retries:
                raise RuntimeError('unexpected recovery count')
            if expected_retries and not info['interruptedPrefixMatches']:
                raise RuntimeError('recovered frame does not match original prefix')
            if options.get('replay') or options.get('replay-prefix-bytes'):
                if not info['replay']['allBytesIdentical'] or info['replay']['additionalExposures']:
                    raise RuntimeError('replay identity failed')
            expected = options.get('width', 3552) * options.get('height', 3552) * 2
            if info['bytes'] != expected or expected > 32 * 1024 * 1024:
                raise RuntimeError('wrong frame size')
            data = exact(expected)
            if hashlib.sha256(data).hexdigest() != info['sha256']:
                raise RuntimeError('IPC image digest mismatch')
            # Omit the two pixels at each end that were replaced from nearby rows.
            pixels = np.frombuffer(data, dtype='<u2')[2:-2]
            info['sensorStatistics'] = {'mean': float(pixels.mean()), 'median': float(np.median(pixels)),
                'minimum': int(pixels.min()), 'maximum': int(pixels.max()),
                'lowFourBitsNonzero': int(np.count_nonzero(pixels & 15)), 'pixels': len(pixels)}
            results.append(metadata)
        if proc.stdout.read(1):
            raise RuntimeError('unexpected bytes after last frame')
        if proc.wait(timeout=5) != 0:
            raise RuntimeError(proc.stderr.read().decode(errors='replace'))
        if len({x['capture']['sha256'] for x in results}) != len(results):
            raise RuntimeError('identical frame digests; freshness needs investigation')
    finally:
        if proc.poll() is None:
            proc.kill()
        proc.wait(timeout=5)
        watchdog.cancel()
        watchdog.join()
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--long', action='store_true', help='also test a 30-second full frame')
    args = parser.parse_args()
    cases = [
        dict(width=64, height=64, microseconds=10000),
        dict(width=520, height=258, microseconds=10000),
        dict(width=512, height=256, microseconds=32, gain=0, offset=10, frames=2),
        dict(width=512, height=256, microseconds=1000, gain=0, offset=10, frames=2),
        dict(width=512, height=256, x=16, y=8, microseconds=100000, gain=100, offset=20, frames=2),
        dict(microseconds=100000, gain=0, offset=10, frames=2),
        dict(microseconds=500000, gain=0, offset=10, frames=2),
        dict(microseconds=999999, gain=0, offset=10, frames=2),
        dict(microseconds=1000000, gain=0, offset=10, frames=2),
        dict(width=512, height=256, microseconds=2000000, gain=180, offset=10, frames=2),
        dict(microseconds=1000000, **{'replay-prefix-bytes':3145728}),
        dict(microseconds=100000, **{'replay-prefix-bytes':12582912}),
        dict(microseconds=100000, **{'replay-prefix-bytes':25165824}),
        dict(width=512, height=256, microseconds=100000, **{'replay-prefix-bytes':131072}),
        dict(microseconds=100000, **{'interrupt-read-after-bytes':12582912, 'replay':True}),
    ]
    if args.long:
        cases.append(dict(microseconds=30000000, gain=0, offset=10))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open('x', encoding='utf-8') as output:
        for options in cases:
            result = {'options': options, 'frames': capture(options)}
            output.write(json.dumps(result) + '\n')
            output.flush()
            print(json.dumps({'options': options, 'completeFrames': len(result['frames']),
                'means': [x['capture']['sensorStatistics']['mean'] for x in result['frames']]}), flush=True)


if __name__ == '__main__':
    main()
