"""Operator-authorized ASI585MM Pro cooling and binned video test; no images saved."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('pipe_fixture', ROOT / 'scripts/test-rust.py')
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--hardware', action='store_true', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    with args.output.open('x', encoding='utf-8') as log, fixture.Worker([
            str(ROOT / 'target/debug/regain-device.exe'), 'zwo', 'camera-direct', '--serve']) as worker:
        worker.timer.cancel()
        worker.timer = threading.Timer(240, worker.process.kill)
        worker.timer.start()
        def record(value):
            log.write(json.dumps(value) + '\n')
            log.flush()
            print(json.dumps(value), flush=True)
        opened = worker.call('open', dict(name='ZWO ASI585MM Pro'))[0]
        caps = {c['type'] for c in opened['controls']}
        assert {8, 15, 16, 17} <= caps and not caps.intersection({21, 22, 23})
        for control in (21, 22, 23):
            worker.call('set', dict(control=control, value=1), error=True)
        previous = {c: worker.call('get', dict(control=c))[0] for c in (16, 17)}
        initial = worker.call('get', dict(control=8))[0] / 10
        target = max(15, min(20, int(initial) - 2))
        samples = []
        try:
            worker.call('set', dict(control=16, value=target))
            worker.call('set', dict(control=17, value=1))
            worker.call('start', dict(width=3840, height=2160, x=0, y=0, bin=1,
                                      microseconds=60000000, dark=False))
            for second in range(100):
                time.sleep(1)
                temperature = worker.call('get', dict(control=8))[0] / 10
                power = worker.call('get', dict(control=15))[0]
                samples.append((temperature, power))
                if second % 5 == 0:
                    record(dict(kind='cooler', second=second, temperature=temperature, power=power, target=target))
                if second == 70:
                    assert worker.call('status')[0] == 2
                    meta, pixels = worker.call('download')
                    assert len(pixels) == 3840 * 2160 * 2
                    assert hashlib.sha256(pixels).hexdigest() == meta['sha256']
                    record(dict(kind='cooled-still', seconds=60, bytes=len(pixels)))
            assert max(p for _, p in samples) > 0, 'cooler demand did not rise'
            assert samples[50][1] > 0 and samples[50][0] < initial - 0.5, 'cooling stalled during exposure'
            assert min(t for t, _ in samples) < initial - 0.5, 'no measured cooling'
            for binning in (1, 2, 3, 4):
                params = dict(mode='video', maxFps=5, width=3840//binning,
                              height=2160//binning, x=0, y=0, bin=binning,
                              microseconds=100000, dark=False)
                for _ in range(2):
                    meta, pixels = worker.frame(params)
                    assert meta['bin'] == binning and meta['bytes'] == len(pixels)
                    assert hashlib.sha256(pixels).hexdigest() == meta['sha256']
                record(dict(kind='cooled-video', bin=binning, bytes=len(pixels)))
            worker.call('stop')
        finally:
            worker.call('stop')
            for control in (17, 16):
                worker.call('set', dict(control=control, value=previous[control]))
                assert worker.call('get', dict(control=control))[0] == previous[control]
            worker.call('close')
            record(dict(kind='restored', controls=previous))
        record(dict(kind='experiment-complete', initial=initial, minimum=min(t for t, _ in samples)))


if __name__ == '__main__':
    main()
