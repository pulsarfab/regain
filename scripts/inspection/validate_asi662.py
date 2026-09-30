"""Opt-in physical ASI662MC SDK-free RAW16/replay matrix; no images are saved."""
import argparse
import json
from pathlib import Path
from validate_direct import capture


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--long', action='store_true', help='Include a 60-second full-frame replay')
    args = parser.parse_args()
    cases = [
        dict(width=64, height=64, microseconds=32),
        dict(width=64, height=66, microseconds=1000),
        dict(width=520, height=258, x=24, y=16, microseconds=10000),
        dict(width=64, height=64, x=1856, y=1016, microseconds=100000),
        dict(width=512, height=256, x=16, y=8, microseconds=100000, frames=3),
    ]
    cases += [dict(width=512, height=256, gain=g, offset=o) for g,o in [(0,0),(199,15),(200,15),(201,300),(600,300)]]
    cases += [dict(microseconds=t, replay=True) for t in [32,1000,100000,999999,1000000,1001000,2000000]]
    cases += [dict(microseconds=t, replay=True, **{'interrupt-read-after-bytes':n})
              for t,n in [(32,1048576),(1000,3145728),(100000,1048576),(1000000,3145728)]]
    cases += [dict(replay=True, **{'replay-prefix-bytes':3145728})]
    if args.long:
        cases += [dict(microseconds=60000000, replay=True)]
    with args.output.open('x', encoding='utf-8') as out:
        for options in cases:
            results = capture(options, '--capture-662', (1920,1080))
            for result in results:
                assert result['productId'] == 0x662b and result['capture']['model'] == 'ASI662MC'
            out.write(json.dumps({'options':options,'results':results})+'\n'); out.flush()
            print(f'PASS {options}', flush=True)
        try:
            capture({'read-retries':0,'interrupt-read-after-bytes':1048576}, '--capture-662', (1920,1080))
        except RuntimeError as error:
            assert 'injected host read interruption' in str(error), str(error)
            out.write(json.dumps({'noRetryFailureSurfaced':True})+'\n')
        else:
            raise AssertionError('zero-retry interruption must fail')


if __name__ == '__main__': main()
