"""Test scoped Cypress USB port operations without Windows elevation.

Disconnect apps and disable cooling. The test requires one ASI2600 P25, performs
reset-port and cycle-port, checks retained-frame readability, then takes a new
frame. No assumption is made that USB power cycling removes external 12 V power.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]


def arrival():
    # Only report model, arrival time and status; omit instance IDs from evidence.
    command = """Get-PnpDevice -PresentOnly | Where-Object InstanceId -like 'USB\\VID_03C3*' | ForEach-Object {
        $p = Get-PnpDeviceProperty -InstanceId $_.InstanceId -KeyName DEVPKEY_Device_LastArrivalDate
        [pscustomobject]@{camera=$_.FriendlyName;arrival=$p.Data.ToUniversalTime().ToString('o');status=$_.Status}
    } | ConvertTo-Json -Compress"""
    result = subprocess.run(['powershell.exe', '-NoProfile', '-Command', command],
                            capture_output=True, text=True, check=True, timeout=20)
    data = json.loads(result.stdout)
    return sorted(data if isinstance(data, list) else [data], key=lambda d: d['camera'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--worker', type=Path, default=ROOT / 'target/release/regain-direct.exe')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    digest = hashlib.sha256(args.worker.read_bytes()).hexdigest()
    results = []

    def run(label, options, timeout=45):
        assert hashlib.sha256(args.worker.read_bytes()).hexdigest() == digest
        process = subprocess.run([str(args.worker), *options], capture_output=True, text=True, timeout=timeout)
        (args.output / f'private-{label}.log').write_text(process.stderr)
        value = json.loads(process.stdout) if process.stdout.strip() else None
        return process.returncode, value, process.stderr

    for operation in ('reset-port', 'cycle-port'):
        code, baseline, _ = run(operation+'-baseline', ['--capture-2600-p25', '--gain', '100', '--offset', '50', '--keep-retained'])
        assert code == 0 and baseline['capture']['bytes'] == 52183296
        before = arrival()
        code, reset, error = run(operation, ['--'+operation+'-2600-p25'])
        assert code == 0 and reset['identityVerified'], error
        after = arrival()
        unchanged = lambda values: [v for v in values if v['camera'] != 'ZWO ASI2600MM Pro Camera']
        assert unchanged(before) == unchanged(after), (before, after)
        if operation == 'cycle-port':
            assert before != after, 'PnP arrival time did not change after cycling the device'
        code, verification, error = run(operation+'-verify', ['--verify-retained-2600-p25',
            '--expected-wire-sha256', baseline['capture']['wireInteriorSha256']])
        # Observed firmware leaves BC23=15 but returns no bytes after either
        # operation. Keep this a regression assertion, not a claimed DDR loss.
        assert code != 0 and verification is None and '"chunk":0' in error and '"category":"timeout"' in error, (verification,error)
        code, fresh, error = run(operation+'-fresh', ['--capture-2600-p25', '--gain', '100', '--offset', '100'])
        assert code == 0 and fresh['capture']['bytes'] == 52183296 and fresh['capture']['offset'] == 100, error
        results.append(dict(operation=operation, before=before, after=after, reset=reset,
            retainedReadSucceeded=False, retainedReadAttempts=3, retainedReadFailure='timeout before first chunk',
            freshFrame=fresh['capture']))
        (args.output / 'results.json').write_text(json.dumps(dict(camera='ASI2600MM Pro P25',
            workerSha256=digest, cases=results), indent=2)+'\n')
        print(f'{operation}: succeeded; retained read unavailable; fresh full frame passed', flush=True)


if __name__ == '__main__':
    main()
