"""Test production retained-handle recovery on one capped ASI2600 P25.

Requires Windows, Frida, and disconnected camera applications. Injects real
CancelIoEx failures; camera initialization and recovery remain production code.
Raw traces stay private; results.json contains no device path or serial.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys

from validate_usb_lifecycle import summarize

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--worker', type=Path, default=ROOT / 'target/release/regain-direct.exe')
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    digest = hashlib.sha256(args.worker.read_bytes()).hexdigest()
    results = []
    cases = [(.1, 2, 2), (.1, 12, 2), (.1, 49, 2), (60, 12, 2),
             (.1, 1, 2), (.1, 12, 0), (.1, 12, 1)]
    for index, (seconds, chunk, retries) in enumerate(cases):
        assert hashlib.sha256(args.worker.read_bytes()).hexdigest() == digest
        trace = args.output / f'private-{index}.jsonl'
        subprocess.run([sys.executable, str(Path(__file__).with_name('trace_direct.py')),
            '--worker', str(args.worker), '--output', str(trace), '--cancel-bulks', str(chunk), str(chunk+1),
            '--', '--capture-2600-p25', '--microseconds', str(int(seconds*1e6)),
            '--gain', '100', '--offset', '50', '--read-retries', str(retries), '--replay'],
            check=True, timeout=seconds+90)
        summary = summarize(trace)
        assert summary['exposureStarts'] == 1, summary
        expect_frame = retries == 2
        assert len(summary['captures']) == int(expect_frame), summary
        if expect_frame:
            meta = summary['captures'][0]
            assert meta['readRecoveries'] == 2 and not meta['sdkLoaded'], meta
            assert meta['replay']['pixelBytesIdentical'], meta
            assert meta['handleReopens'] == int(chunk > 1), meta
            assert meta['verifiedRetainedBytes'] == max(0, (chunk-1)*1048576-4), meta
            assert summary['handleOpens'] == 1 + int(chunk > 1), summary
            if chunk > 1:
                assert summary['exposureStartsAfterReopen'] == 0, summary
                assert summary['sensorWritesAfterReopenBeforeRead'] == 0, summary
        else:
            assert summary['handleOpens'] == 1, summary
        assert len(summary['failures']) == min(retries+1, 2), summary
        assert all(f['category'] == 'cancelled' and f['terminalCompletionObserved'] for f in summary['failures']), summary
        results.append(dict(seconds=seconds, cancelBulk=chunk, readRetries=retries, **summary))
        (args.output / 'results.json').write_text(json.dumps(dict(camera='ASI2600MM Pro P25',
            workerSha256=digest, cases=results), indent=2)+'\n')
        print(f'case {index}: {seconds}s, readRetries={retries}, frame={expect_frame}', flush=True)


if __name__ == '__main__':
    main()
