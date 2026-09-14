"""Build-time protocol checks against the real packaged host, using no hardware."""
import time

from client import Host, HostError


def verify_host(executable, sdk):
    host = Host(executable, sdk, simulate=True)
    try:
        host.call('open', dict(name='ZWO Simulated'))
        saved = dict(control=0, value=73, auto=True)
        assert host.call('set-control-state', saved)[0] == dict(value=73, auto=True)
        assert host.call('get-control-state', dict(control=0))[0] == dict(value=73, auto=True)
        exposure = dict(width=64, height=64, bin=1, x=0, y=0, microseconds=1000, dark=True)
        host.call('start', exposure)
        host.call('fault', dict(kind='download'))
        try:
            host.call('download')
            raise AssertionError('Simulated SDK error was accepted')
        except HostError as error:
            assert 'ASI error 11' in str(error)
        assert host.call('download')[1]['bytes'] == 64 * 64 * 2
        host.call('start', exposure)
        host.call('fault', dict(kind='hang'))
        start = time.monotonic()
        try:
            host.call('download', timeout=.2)
            raise AssertionError('Host hang did not hit deadline')
        except HostError as error:
            assert 'deadline' in str(error)
        host.proc.wait(timeout=5)
        assert time.monotonic() - start < 6
    finally:
        host.dispose()
    print('Packaged host: auto-state round trip, SDK failure and hung-download deadline passed.', flush=True)
