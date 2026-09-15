"""ASI6200MM Pro P25 direct RAW16 dark-frame matrix; save statistics, never pixels."""
import argparse
import json
from pathlib import Path
from validate_duo import capture


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--long', type=int, default=0, help='append a full-frame exposure, up to 2000 seconds')
    args = parser.parse_args()
    if not 0 <= args.long <= 2000:
        parser.error('long duration must be 0..2000 seconds')
    base = dict(width=512, height=256, microseconds=100000, gain=100, offset=50, replay=True)
    cases = [dict(base, width=9576, height=6388, frames=3), dict(base, width=64, height=64),
             dict(base, x=16, y=2), dict(base, x=9056, y=6132)]
    for bin, width, height in [(2,4784,3194), (3,3192,2128), (4,2392,1596)]:
        cases.append(dict(base, width=width, height=height, bin=bin))
    for gain in [0,60,61,62,99,100,101,159,160,161,279,280,281,460,461,520,521,700]:
        cases.append(dict(base, gain=gain))
    for us in [32,1000,999999,1000000,1001000,2000000,10000000,60000000]:
        cases.append(dict(base, microseconds=us))
    for offset in [0,200]:
        cases.append(dict(base, offset=offset))
    for fault in ['interrupt-read-after-bytes','timeout-read-after-bytes','replay-prefix-bytes']:
        cases.append(dict(base, width=9576, height=6388, **{fault:12*1024*1024}))
    if args.long:
        cases.append(dict(base, width=9576, height=6388, microseconds=args.long*1000000))
    with args.output.open('x', encoding='utf-8') as output:
        for i, options in enumerate(cases):
            results = capture(options, asi6200=True)
            output.write(json.dumps({'case':i, 'options':options, 'captures':results})+'\n')
            output.flush()
            print(f'case {i}: {len(results)} frames, recovery {[r["readRecoveries"] for r in results]}', flush=True)


if __name__ == '__main__':
    main()
