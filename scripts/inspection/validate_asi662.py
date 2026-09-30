"""Opt-in physical ASI662MC SDK-free RAW16/replay matrix; no images are saved."""
import argparse
import json
from pathlib import Path
from validate_direct import capture


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--long', action='store_true', help='Include a 60-second full-frame replay')
    parser.add_argument('--extended-only', action='store_true', help='Run two 600-second captures with replay/recovery, then a short capture')
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
    if args.extended_only:
        cases = [
            dict(microseconds=600000000, replay=True),
            dict(microseconds=600000000, replay=True,
                 **{'interrupt-read-after-bytes':1048576, 'replay-prefix-bytes':3145728}),
            dict(width=64, height=64, microseconds=1000),
        ]
    digests = set()
    with args.output.open('x', encoding='utf-8') as out:
        for options in cases:
            print(f'START {options}', flush=True)
            results = capture(options, '--capture-662', (1920,1080))
            for result in results:
                assert result['productId'] == 0x662b and result['capture']['model'] == 'ASI662MC'
                digest = result['capture']['sha256']
                assert digest not in digests, 'identical frame digests; freshness needs investigation'
                digests.add(digest)
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
