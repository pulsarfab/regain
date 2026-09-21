"""Exercise the NINA private-pipe/Rust recovery path with real USB read faults.

Uses an owned supervisor and Frida child gating to observe its direct worker
before camera open. Never attaches to NINA or other existing camera processes.
The capped ASI2600 P25 or ASI6200 must be released by other applications.
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
    parser.add_argument('--workers', type=Path, default=ROOT / 'target/release')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--camera-name', choices=['ZWO ASI2600MM Pro', 'ZWO ASI6200MM Pro'], default='ZWO ASI2600MM Pro')
    args = parser.parse_args()
    is_6200 = args.camera_name == 'ZWO ASI6200MM Pro'
    width, height = (9576, 6388) if is_6200 else (6248, 4176)
    args.output.mkdir(parents=True, exist_ok=False)
    results = []
    for index, (seconds, reads, expect_success) in enumerate([(60, 2, True), (31, 1, False)]):
        events, children, errors = [], [], []
        command = [str(args.workers / 'regain-alpaca.exe'), '--stdio', '--backend', 'direct',
                   '--workers', str(args.workers.resolve())]
        with ProtocolWorker(command) as worker:
            worker.timer.cancel()
            worker.timer = threading.Timer(150, worker.process.kill)
            worker.timer.start()
            device = frida.get_local_device()
            parent = device.attach(worker.process.pid)

            def inspect_child(child):
                if child.parent_pid != worker.process.pid:
                    return
                try:
                    if Path(child.path).name.lower() != 'regain-device.exe':
                        raise RuntimeError(f'unexpected worker {Path(child.path).name}')
                    attached = device.attach(child.pid)
                    cancellations = [12] if is_6200 and expect_success else [12, 13]
                    source = f'globalThis.TRACE_CANCEL_BULK_NUMBERS = {json.dumps(cancellations)};\n'
                    script = attached.create_script(source + Path(__file__).with_name('trace-transport.js').read_text())
                    script.on('message', lambda message, data: events.append(message.get('payload', message)))
                    script.load()
                    children.append((attached, script))
                except Exception as error:
                    errors.append(str(error))
                finally:
                    device.resume(child.pid)

            def child_added(child):
                # Frida invokes callbacks on its dispatcher. Attach/resume must
                # run elsewhere so their synchronous replies can be dispatched.
                threading.Thread(target=inspect_child, args=(child,), daemon=True).start()

            device.on('child-added', child_added)
            parent.enable_child_gating()
            print(f'Opening owned supervisor for {seconds}s test', flush=True)
            try:
                # Fallback is enabled; a long exposure must still never be replaced.
                worker.call('open', dict(name=args.camera_name, allowSdkFallback=True,
                    recovery=dict(directReadRetries=reads, maximumRetryExposureSeconds=30)))
                assert len(children) == 1 and not errors, errors
                worker.call('set', dict(control=0, value=100))
                worker.call('set', dict(control=5, value=50))
                worker.call('start', dict(width=width, height=height, x=0, y=0, bin=1,
                    microseconds=int(seconds*1e6), dark=True))
                until = time.monotonic()+seconds+60
                while True:
                    state, _ = worker.call('status')
                    if state['state'] != 1:
                        break
                    assert time.monotonic() < until, state
                    time.sleep(.1)
                assert state['state'] == (2 if expect_success else 3), state
                metadata = None
                if expect_success:
                    metadata, pixels = worker.call('download')
                    assert len(pixels) == width * height * 2 and hashlib.sha256(pixels).hexdigest() == metadata['sha256']
                    assert metadata['recoveries'] == 0 and metadata.get('handleReopens', 0) == (0 if is_6200 else 1)
                    assert 1 <= metadata['readRecoveries'] <= 2 and not metadata['sdkFallback']
                else:
                    reply, pixels = worker.raw('download')
                    assert not reply['ok'] and not pixels
                starts = [e for e in events if e.get('kind') == 'io-submit' and (e.get('header') or '').startswith('40a9')]
                assert len(starts) == 1 and len(children) == 1 and not errors, (len(starts), len(children), errors)
                injected = [e for e in events if e.get('kind') == 'injected-cancel']
                expected_cancellations = 1 if is_6200 and expect_success else 2
                assert len(injected) == expected_cancellations and all(e['ok'] for e in injected)
                for cancellation in injected:
                    terminal = [e for e in events if e.get('kind') == 'io-complete'
                                and e['sequence'] == cancellation['sequence']]
                    assert len(terminal) == 1 and terminal[0]['lastError'] == 995 and not terminal[0]['ok']
                worker.log.flush()
                worker.log.seek(0)
                logs = worker.log.read().decode(errors='replace')
                if expect_success:
                    assert 'capture.recovered' in logs, logs
                    if not is_6200:
                        assert 'transfer.reopening' in logs and '1 handle reopens' in logs, logs
                assert 'backend.fallback' not in logs
                results.append(dict(seconds=seconds, readRetries=reads, success=expect_success,
                    injectedBulks=[12] if is_6200 and expect_success else [12, 13],
                    exposureStarts=len(starts), workersStarted=len(children), sdkFallback=False,
                    recoveryLogForwarded=expect_success, frame=metadata))
            finally:
                worker.call('close')
                parent.disable_child_gating()
                device.off('child-added', child_added)
                parent.detach()
                (args.output / f'private-{index}.jsonl').write_text(''.join(json.dumps(e)+'\n' for e in events))
        (args.output / 'results.json').write_text(json.dumps(dict(camera=args.camera_name,
            builds={n:hashlib.sha256((args.workers/n).read_bytes()).hexdigest() for n in ('regain-alpaca.exe', 'regain-device.exe')},
            cases=results), indent=2)+'\n')
        print(f'{seconds}s: expected success={expect_success}, one exposure, no SDK fallback', flush=True)


if __name__ == '__main__':
    main()
