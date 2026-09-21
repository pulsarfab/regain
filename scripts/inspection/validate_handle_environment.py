"""Verify real P25 cooler/aux state through production handle recovery.

Disconnect all camera apps first. Runs an owned direct worker, applies cooling
and dew/fan/LED settings, cancels two reads, and checks a recovered full frame.
Finally restores the initial auxiliary settings and disables the cooler.
"""
import argparse
import hashlib
import json
from pathlib import Path
import threading
import time

import frida
from validate_transfer_deadline import ProtocolWorker

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--worker', type=Path, default=ROOT / 'target/release/regain-device.exe')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    events = []
    with ProtocolWorker([str(args.worker), '--serve']) as worker:
        worker.timer.cancel()
        worker.timer = threading.Timer(120, worker.process.kill)
        worker.timer.start()
        session = frida.attach(worker.process.pid)
        script = session.create_script('globalThis.TRACE_CANCEL_BULK_NUMBERS = [12,13];\n' +
            Path(__file__).with_name('trace-transport.js').read_text())
        script.on('message', lambda message, data: events.append(message.get('payload', message)))
        script.load()
        worker.call('open', dict(name='ZWO ASI2600MM Pro'))
        initial = {}

        def get(kind):
            return worker.call('get', dict(control=kind))[0]

        def put(kind, value):
            worker.call('set', dict(control=kind, value=value))

        try:
            initial = {k: get(k) for k in (16, 17, 21, 22, 23)}
            target = max(-10, min(30, int(get(8)/10)-5))
            desired = {16: target, 17: 1, 21: 1, 22: 192, 23: 128}
            for k, value in desired.items():
                put(k, value)
            until = time.monotonic()+60
            while get(15) <= 10:
                assert time.monotonic() < until, 'cooler never developed measurable output'
                time.sleep(1)
            before = {k: get(k) for k in (8, 15, *desired)}
            put(0, 100)
            put(5, 50)
            exposure = dict(width=6248, height=4176, x=0, y=0, bin=1,
                            microseconds=2000000, dark=True, readRetries=2)
            worker.call('start', exposure)
            assert worker.wait_capture()['ok']
            metadata, pixels = worker.call('download')
            assert len(pixels) == 52183296 and hashlib.sha256(pixels).hexdigest() == metadata['sha256']
            assert metadata['handleReopens'] == 1 and metadata['readRecoveries'] == 2, metadata
            assert metadata['verifiedRetainedBytes'] == 11*1048576-4, metadata
            after = {k: get(k) for k in before}
            assert all(after[k] == v for k, v in desired.items()), after
            assert after[15] > 0 and abs(after[8]-before[8]) <= 25, (before, after)
            # A subsequent request uses the preserved setpoint and fresh settings.
            put(5, 100)
            worker.call('start', exposure)
            assert worker.wait_capture()['ok']
            next_meta, next_pixels = worker.call('download')
            assert next_meta['handleReopens'] == 0 and next_meta['offset'] == 100
            assert len(next_pixels) == len(pixels) and next_meta['sha256'] != metadata['sha256']
            opens = [e for e in events if e.get('kind') == 'device-open']
            starts = [e for e in events if e.get('kind') == 'io-submit' and (e.get('header') or '').startswith('40a9')]
            assert len(opens) == 2 and len(starts) == 2, (len(opens), len(starts))
            result = dict(camera='ASI2600MM Pro P25', workerSha256=hashlib.sha256(args.worker.read_bytes()).hexdigest(),
                before={str(k):v for k,v in before.items()}, after={str(k):v for k,v in after.items()},
                recoveredFrame=metadata, subsequentFrame=next_meta,
                exposureStarts=len(starts), handleOpens=len(opens))
            (args.output / 'results.json').write_text(json.dumps(result, indent=2)+'\n')
        finally:
            try:
                put(17, 0)
                for k, value in initial.items():
                    if k != 17:
                        put(k, value)
                worker.call('close')
            finally:
                script.unload()
                session.detach()
                (args.output / 'private-trace.jsonl').write_text(''.join(json.dumps(e)+'\n' for e in events))
    print('Recovered full frame; cooler, setpoint, dew, fan and LED state preserved; subsequent frame passed.')


if __name__ == '__main__':
    main()
