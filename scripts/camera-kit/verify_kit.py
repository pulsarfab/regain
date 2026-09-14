"""Build-time failure/cleanup checks with real simulator processes and tracer."""
import json
from unittest.mock import patch

import camera_kit
from client import Host


def verify_kit(kit, output):
    for failure in ('download', 'crash', 'cancel'):
        processes = []
        injected = False

        class FaultHost(Host):
            def __init__(self, executable, sdk, simulate=False):
                assert simulate, 'These tests must never open hardware'
                super().__init__(executable, sdk, simulate)
                processes.append(self.proc)

            def call(self, method, *args, **kwargs):
                nonlocal injected
                if method == 'download' and not injected:
                    injected = True
                    if failure == 'cancel':
                        raise KeyboardInterrupt()
                    super().call('fault', dict(kind=failure))
                return super().call(method, *args, **kwargs)

        dest = output / failure
        paths = (kit, kit / 'zwogain-host.exe', kit / 'ASICamera2.dll', kit / 'source/trace-transport.js')
        with patch.object(camera_kit, 'Host', FaultHost), patch.object(camera_kit, 'paths', return_value=paths):
            code = camera_kit.main(['--self-test', '--include-pixels', '--output', str(dest)])
        assert code == (130 if failure == 'cancel' else 2), (failure, code)
        manifests = list(dest.glob('*/manifest.json'))
        assert len(manifests) == 1
        saved = json.loads(manifests[0].read_text())
        assert not saved['passed']
        assert saved['completed'] == (failure == 'download')
        assert saved['restoration'] and all(c['matched'] for c in saved['restoration'].values())
        assert len(list(dest.glob('*.zip'))) == 1
        assert all(p.poll() is not None for p in processes)
        if failure == 'crash':
            assert len(processes) == 2, 'Crash cleanup must reopen the same camera'
    print('Kit failure checks: partial evidence, cancellation, worker crash and restoration passed.', flush=True)
