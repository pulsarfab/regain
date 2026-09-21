"""Exercise every accessory selector through the one packaged hardware executable."""
import argparse
import json
import os
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin-dir', default='target/debug')
    args = parser.parse_args()
    executable = str(Path(args.bin_dir).resolve() / ('regain-device.exe' if os.name == 'nt' else 'regain-device'))
    help_text = subprocess.check_output([executable, '--help'], text=True, timeout=10)
    assert 'VENDOR DEVICE' in help_text
    bad = subprocess.run([executable, 'unknown', 'unknown'], capture_output=True, timeout=10)
    assert bad.returncode != 0 and b'Unknown device' in bad.stderr
    for vendor, device in [('zwo', 'caa'), ('zwo', 'efw'), ('zwo', 'eaf'), ('pegasus', 'fc3'), ('deepskydad', 'ofp2'), ('wanderer', 'eta')]:
        command = [executable, vendor, device]
        entries = json.loads(subprocess.check_output(command + ['list-details', '--simulate'], timeout=10))
        assert len(entries) == 1, (vendor, device, entries)
        identity = entries[0].get('identity', entries[0])
        serve = command + ['serve', '--serial', identity['serial'], '--simulate']
        result = subprocess.run(serve, input=b'\xef\xbb\xbf{"command":"identity"}\ninvalid-json\n{"command":"status"}\n', capture_output=True, timeout=10, check=True)
        replies = [json.loads(line) for line in result.stdout.splitlines()]
        assert [r['ok'] for r in replies] == [True, False, True], (device, replies, result.stderr)
        assert replies[0]['result']['serial'] == identity['serial']
        # An unterminated or oversized request must not be processed on EOF.
        for payload in [b'{"command":"status"}', b'x' * 4097 + b'\n{"command":"status"}\n']:
            result = subprocess.run(serve, input=payload, capture_output=True, timeout=10, check=True)
            assert not result.stdout, (device, result.stdout)
    print('Unified worker: all six accessory selectors, BOM, error recovery, bounded input and EOF passed')


if __name__ == '__main__':
    main()
