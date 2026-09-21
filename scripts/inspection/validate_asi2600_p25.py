"""ASI2600MM Pro P25 direct dark-frame matrix, including full-frame freshness."""
import argparse
import hashlib
import json
from pathlib import Path
from validate_duo import ROOT, capture


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--long', type=int, default=0, help='additional full-frame exposure, 0..2000 seconds')
    parser.add_argument('--worker', type=Path)
    parser.add_argument('--full-rows-only', action='store_true', help='only the ten full-frame offset/timing transitions')
    args = parser.parse_args()
    if not 0 <= args.long <= 2000:
        parser.error('long duration must be 0..2000 seconds')
    worker = args.worker or ROOT / 'target/debug/regain-device.exe'
    digest = hashlib.sha256(worker.read_bytes()).hexdigest()
    base = dict(width=512, height=256, microseconds=100000, gain=100, offset=50, replay=True)
    cases = []
    for us in [32, 100000, 999999, 1000000, 2000000]:
        for offset in [240, 50]:
            cases.append(dict(base, width=6248, height=4176, microseconds=us, offset=offset))
    cases += [dict(base, width=6248, height=4176, frames=3),
              dict(base, width=64, height=64), dict(base, x=16, y=2),
              dict(base, x=5728, y=3920)]
    for bin, width, height in [(2, 3120, 2088), (3, 2080, 1392), (4, 1560, 1044)]:
        cases.append(dict(base, width=width, height=height, bin=bin))
    for gain in [-25, -1, 0, 99, 100, 101, 460, 461, 520, 521, 700]:
        cases.append(dict(base, gain=gain))
    for us in [32, 1000, 999999, 1000000, 1001000, 2000000, 10000000, 60000000]:
        cases.append(dict(base, microseconds=us))
    for fault in ['interrupt-read-after-bytes', 'timeout-read-after-bytes', 'replay-prefix-bytes']:
        cases.append(dict(base, width=6248, height=4176, **{fault: 12 * 1024 * 1024}))
    if args.full_rows_only:
        cases = cases[:10]
    if args.long:
        cases.append(dict(base, width=6248, height=4176, microseconds=args.long * 1000000))
    with args.output.open('x', encoding='utf-8') as output:
        for i, options in enumerate(cases):
            if hashlib.sha256(worker.read_bytes()).hexdigest() != digest:
                raise RuntimeError('worker changed during hardware tests')
            results = capture(options, worker=worker, asi2600_p25=True)
            valid = True
            if options['width'] == 6248 and options['gain'] == 100 and options['microseconds'] <= 2000000:
                valid = all(abs(value - options['offset'] * 10) <= 15
                            for result in results for value in result['statistics']['rowMedianRange'])
            output.write(json.dumps(dict(case=i, workerSha256=digest, options=options,
                                         captures=results, freshRows=valid)) + '\n')
            output.flush()
            if not valid:
                raise RuntimeError('offset did not reach all row bands; inspect saved statistics')
            print(f'case {i}: {len(results)} frames, recovery {[r["readRecoveries"] for r in results]}', flush=True)


if __name__ == '__main__':
    main()
