"""Operator-authorized ASI585MM Pro direct read-size/replay acceptance; saves no images."""
import argparse
import hashlib
import json
from pathlib import Path
import statistics
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--hardware', action='store_true', required=True)
    parser.add_argument('--worker', type=Path, default=ROOT / 'target/debug/regain-device.exe')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    command = [str(args.worker.resolve()), 'zwo', 'camera-direct']
    probe = json.loads(subprocess.check_output(command + ['--probe-pid', '585e'], timeout=30))
    summary = dict(probe=probe, workerSha256=hashlib.sha256(args.worker.read_bytes()).hexdigest(), runs=[])
    for size in [1, 4, 16, 64, 256, 1024, None]:
        options = [] if size is None else ['--read-chunk-kib', str(size)]
        completed = subprocess.run(command + ['--capture-585', '--frames', '3', '--microseconds', '100000',
            '--interrupt-read-after-bytes', '1048576', '--read-retries', '2', '--replay',
            '--replay-prefix-bytes', '4096', '--transfer-timeout-seconds', '20'] + options,
            capture_output=True, timeout=90)
        label = 'default' if size is None else str(size)
        (args.output / f'{label}.jsonl').write_bytes(completed.stdout)
        (args.output / f'{label}.log').write_bytes(completed.stderr)
        completed.check_returncode()
        frames = [json.loads(line) for line in completed.stdout.splitlines()]
        assert len(frames) == 3
        for frame in frames:
            capture = frame['capture']
            assert not frame['sdkLoaded'] and capture['bytes'] == 16588800
            assert capture['readChunkKiB'] == (size or 1024)
            assert capture['replay']['allBytesIdentical'] and capture['replay']['additionalExposures'] == 0
            assert capture['interruptedPrefixMatches'] and capture['readoutRetriesUsed'] == 1
        reads = []
        for line in completed.stderr.decode(errors='replace').splitlines():
            if line.startswith('REGAIN_DIAGNOSTIC '):
                record = json.loads(line[len('REGAIN_DIAGNOSTIC '):])
                if record['event'] == 'transfer.complete' and record['details']['frameBytes'] == 16588800:
                    detail = record['details']
                    assert detail['readChunkKiB'] == (size or 1024)
                    assert detail['requests'] == (16588800 + (size or 1024)*1024-1)//((size or 1024)*1024)
                    reads.append(detail)
        assert len(reads) == 6
        result = dict(readChunkKiB=size or 1024, omitted=size is None, captures=3,
            identicalReplays=3, recoveredHostInterruptions=3,
            medianFullReadMs=statistics.median(r['elapsedUs']/1000 for r in reads), reads=reads)
        summary['runs'].append(result)
        (args.output / 'results.json').write_text(json.dumps(summary, indent=2))
        print(json.dumps({k:v for k,v in result.items() if k != 'reads'}), flush=True)


if __name__ == '__main__':
    main()
