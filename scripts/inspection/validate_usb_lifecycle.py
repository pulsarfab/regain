"""ASI2600 P25 retained-frame reopen experiments (Windows, Frida, capped camera).

Disconnect camera apps first. Writes private USB traces beside sanitized results.
No power cycle, firmware write, or automatic replacement exposure is performed.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]


def summarize(path):
    events = [json.loads(line) for line in path.read_text().splitlines()]
    stdout = ''.join(e['text'] for e in events if e['kind'] == 'worker-output' and e['fd'] == 1)
    stderr = ''.join(e['text'] for e in events if e['kind'] == 'worker-output' and e['fd'] == 2)
    replies = [json.loads(line) for line in stdout.splitlines() if line.strip()]
    captures = [r['capture'] for r in replies if 'capture' in r]
    diagnostics = [json.loads(line.removeprefix('ZWOGAIN_DIAGNOSTIC '))
                   for line in stderr.splitlines() if line.startswith('ZWOGAIN_DIAGNOSTIC ')]
    submits = [e for e in events if e['kind'] == 'io-submit']
    starts = [e for e in submits if (e.get('header') or '').startswith('40a9')]
    opens = [e for e in events if e['kind'] == 'device-open']
    # Initialization/AA must never occur between the close and recovered frame.
    reopen_time = opens[1]['timeMs'] if len(opens) == 2 else None
    before_read = []
    if len(opens) == 2:
        after_open = events[events.index(opens[1]) + 1:]
        for event in after_open:
            if event['kind'] == 'io-submit':
                if event['code'] == '0x22004b':
                    break
                before_read.append(event)
    return dict(captures=captures, verifications=[r['retainedVerification'] for r in replies if 'retainedVerification' in r],
                exposureStarts=len(starts), handleOpens=len(opens),
                processExit=next((dict(bulkNumber=e['bulkNumber'], bytes=e['bytes'])
                                  for e in events if e['kind'] == 'injected-process-exit'), None),
                sensorWritesAfterReopenBeforeRead=sum((e.get('header') or '').startswith('40b6') for e in before_read),
                reopened=next((e['details'] for e in diagnostics if e['event'] == 'research.reopened'), None),
                failures=[e['details'] for e in diagnostics if e['event'] == 'transfer.failed'],
                phases=[e['details']['phase'] for e in diagnostics if e['event'] == 'capture.phase'],
                exposureStartsAfterReopen=sum(e['timeMs'] >= reopen_time for e in starts) if reopen_time else None)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True, help='new directory')
    parser.add_argument('--worker', type=Path, default=ROOT / 'target/release/zwogain-direct.exe')
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    digest = hashlib.sha256(args.worker.read_bytes()).hexdigest()
    # First packet, middle and final full chunk; streaming and host-timed modes.
    cases = [(100000, 1024, 0), (100000, 12582912, 1000),
             (100000, 51380224, 5000), (2000000, 12582912, 1000),
             (60000000, 12582912, 5000)]
    results = []
    for index, (us, prefix, delay) in enumerate(cases):
        if hashlib.sha256(args.worker.read_bytes()).hexdigest() != digest:
            raise RuntimeError('worker changed during test')
        trace = args.output / f'private-trace-{index}.jsonl'
        options = ['--capture-2600-p25', '--microseconds', str(us), '--gain', '100',
                   '--offset', '50', '--reopen-after-bytes', str(prefix),
                   '--reopen-delay-ms', str(delay), '--replay']
        subprocess.run([sys.executable, str(Path(__file__).with_name('trace_direct.py')),
                        '--worker', str(args.worker), '--output', str(trace), '--', *options],
                       check=True, timeout=us / 1e6 + 90)
        summary = summarize(trace)
        result = dict(microseconds=us, prefixBytes=prefix, reopenDelayMs=delay, **summary)
        results.append(result)
        (args.output / 'results.json').write_text(json.dumps(dict(
            camera='ASI2600MM Pro P25', workerSha256=digest, cases=results), indent=2) + '\n')
        assert summary['exposureStarts'] == 1 and summary['handleOpens'] == 2, summary
        assert summary['exposureStartsAfterReopen'] == 0, summary
        assert summary['sensorWritesAfterReopenBeforeRead'] == 0, summary
        assert len(summary['captures']) == 1, summary
        frame = summary['captures'][0]
        assert frame['reopenInjectionBytes'] == prefix and frame['interruptedPrefixPixelsMatch'], frame
        assert frame['replay']['pixelBytesIdentical'] and not frame['sdkLoaded'], frame
        print(f'case {index}: same frame recovered after handle reopen; {frame["elapsedMs"]} ms', flush=True)

    replacements = []
    for abrupt in [False, True]:
        def run_step(label, options, trace_flags=()):
            if hashlib.sha256(args.worker.read_bytes()).hexdigest() != digest:
                raise RuntimeError('worker changed during test')
            trace = args.output / f'private-worker-{int(abrupt)}-{label}.jsonl'
            subprocess.run([sys.executable, str(Path(__file__).with_name('trace_direct.py')),
                            '--worker', str(args.worker), '--output', str(trace), *trace_flags,
                            '--', *options], check=True, timeout=90)
            return summarize(trace)
        original = run_step('capture', ['--capture-2600-p25', '--microseconds', '2000000',
                                       '--gain', '100', '--offset', '50', '--keep-retained'])
        assert original['exposureStarts'] == 1 and len(original['captures']) == 1, original
        expected = original['captures'][0]['wireInteriorSha256']
        verification = ['--verify-retained-2600-p25', '--expected-wire-sha256', expected]
        interrupted = None
        if abrupt:
            interrupted = run_step('killed', verification, ['--exit-after-bulk', '12'])
            assert interrupted['processExit'] == dict(bulkNumber=12, bytes=1048576), interrupted
            assert not interrupted['verifications'] and interrupted['exposureStarts'] == 0, interrupted
        recovered = run_step('recovered', verification)
        assert recovered['exposureStarts'] == 0 and len(recovered['verifications']) == 1, recovered
        assert recovered['verifications'][0]['pixelBytesIdentical'], recovered
        replacements.append(dict(abruptReaderExit=abrupt, original=original, interrupted=interrupted, recovered=recovered))
        (args.output / 'results.json').write_text(json.dumps(dict(
            camera='ASI2600MM Pro P25', workerSha256=digest, cases=results,
            workerReplacement=replacements), indent=2) + '\n')
        print(f'worker replacement (reader crash={abrupt}): full raw interior hash matched', flush=True)


if __name__ == '__main__':
    main()
