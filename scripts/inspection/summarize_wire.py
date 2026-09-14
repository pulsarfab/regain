"""Publish compact comparison evidence without device paths, serials or pixels."""
import argparse
import json
from pathlib import Path
from summarize_trace import summarize


def evidence(path):
    summary = summarize(path)
    rows = [json.loads(line) for line in path.read_text().splitlines()]
    config = next(r for r in rows if r['kind'] == 'configuration')
    parameters = {k: v for k, v in config['parameters'].items() if k != 'output'}
    camera = next(r for r in rows if r['kind'] == 'camera')
    comparisons = []
    for entry in summary['wireComparisons']:
        comparisons.append({k: v for k, v in entry.items() if k in [
            'kind', 'stage', 'run', 'wireBytes', 'frameBytes', 'comparable', 'differences']}
            | ({'bestMapping': entry['candidates'][0]} if 'candidates' in entry else {}))
    return {k: summary[k] for k in ['trace', 'traceSha256', 'injectedCancellations', 'attempts',
                                   'wireRuns', 'wireReplayIdentity']} | {
        'parameters': parameters, 'sdkSha256': config['sdkSha256'],
        'camera': {k: camera[k] for k in ['name', 'width', 'height']}, 'comparisons': comparisons}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('trace', type=Path, nargs='+')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    with args.output.open('x', encoding='utf-8') as out:
        json.dump([evidence(path) for path in args.trace], out, indent=2)
        out.write('\n')
