"""Cancel the first/last real ASI6200 USB read and verify retained-frame recovery.

Requires Windows, Frida, a capped camera, and no other owner. Starts only owned
workers; saves private traces and a sanitized result. Does not reset the port.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys


def verify_known_pixels(events):
    hashes = {e['sequence']: (e['bytes'], e['interiorSha256'])
              for e in events if e['kind'] == 'bulk-hash'}
    complete, partial, run, size = [], [], [], 0
    for event in events:
        if event['kind'] != 'io-submit' or event['code'] != '0x22004b':
            continue
        chunk = hashes.get(event['sequence'])
        if chunk is None:
            if run:
                partial.append(run)
            run, size = [], 0
            continue
        size += chunk[0]
        assert size <= 122342976, 'invalid transfer boundary'
        run.append(chunk)
        if size == 122342976:
            complete.append(run)
            run, size = [], 0
    assert len(complete) == 2 and complete[0] == complete[1]
    for prefix in partial:
        assert prefix == complete[0][:len(prefix)], 'downloaded pixels changed after cancellation'
    # Hash instrumentation excludes four bytes at each end of every chunk.
    return sum(length - 8 for prefix in partial for length, _ in prefix)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--worker', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    results = []
    for bulk in (1, 117):
        trace = args.output / f'private-cancel-{bulk}.jsonl'
        subprocess.run([sys.executable, str(Path(__file__).with_name('trace_direct.py')),
                        '--worker', str(args.worker), '--output', str(trace),
                        '--cancel-bulk', str(bulk), '--hash-bulk', '--',
                        '--capture-6200', '--gain', '100', '--offset', '50', '--replay'],
                       check=True, timeout=90)
        events = [json.loads(line) for line in trace.read_text().splitlines()]
        injected = [e for e in events if e['kind'] == 'injected-cancel']
        assert len(injected) == 1 and injected[0]['ok']
        terminal = [e for e in events if e['kind'] == 'io-complete'
                    and e['sequence'] == injected[0]['sequence']]
        assert len(terminal) == 1 and terminal[0]['lastError'] == 995 and not terminal[0]['ok']
        starts = [e for e in events if e['kind'] == 'io-submit'
                  and (e.get('header') or '').startswith('40a9')]
        assert len(starts) == 1, 'download recovery took a new exposure'
        stdout = ''.join(e['text'] for e in events if e['kind'] == 'worker-output' and e['fd'] == 1)
        frames = [json.loads(line)['capture'] for line in stdout.splitlines() if line.startswith('{')]
        assert len(frames) == 1
        frame = frames[0]
        assert frame['bytes'] == 122342976 and not frame['sdkLoaded']
        assert 1 <= frame['readRecoveries'] <= 2
        assert frame['replay']['pixelBytesIdentical']
        results.append(dict(bulk=bulk, cancelled=True, terminalError=995,
                            exposureStarts=1, comparedPrefixBytes=verify_known_pixels(events), frame=frame))
        print(f'Bulk {bulk}: recovered and replayed identical pixels after real USB cancellation', flush=True)
    (args.output / 'results.json').write_text(json.dumps(dict(
        workerSha256=hashlib.sha256(args.worker.read_bytes()).hexdigest(), cases=results), indent=2)+'\n')


if __name__ == '__main__':
    main()
