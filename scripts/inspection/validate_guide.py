"""Exercise Duo guide RAW16 directly; keep only statistics and digests."""
import argparse
import json
from pathlib import Path
from validate_duo import capture


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args()
    cases=[
        dict(width=64,height=64,microseconds=32,gain=0,offset=200),
        dict(width=512,height=256,x=16,y=32,microseconds=100000,gain=100,offset=200,frames=3),
        dict(microseconds=100000,gain=0,offset=200,frames=3),
        dict(width=960,height=540,bin=2,microseconds=100000,gain=100,offset=200,frames=2),
        dict(microseconds=999999,gain=349,offset=200),
        dict(microseconds=1000000,gain=350,offset=200),
        dict(microseconds=2000000,gain=400,offset=400),
        dict(microseconds=10000000,gain=600,offset=1500),
    ]
    with args.output.open('x',encoding='utf-8') as output:
        failures=0
        for i,options in enumerate(cases):
            try:
                results=capture(options,guide=True)
                result={'captures':results}
                print(f'case {i}: {len(results)} frames',flush=True)
            except RuntimeError as error:
                expected=i==0 and 'guide zero-line integrations are not validated' in str(error)
                failures+=not expected
                result={'error':str(error),'expectedRejection':expected}
                print(f'case {i}: {"expected rejection" if expected else "failed"}',flush=True)
            output.write(json.dumps({'case':i,'options':options,**result})+'\n')
            output.flush()
    if failures:
        raise SystemExit(f'{failures} cases failed; see results')


if __name__=='__main__':
    main()
