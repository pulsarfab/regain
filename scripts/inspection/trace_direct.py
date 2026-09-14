"""Trace one owned SDK-free worker, with bounded metadata-only stdout."""
import argparse
import hashlib
import json
from pathlib import Path
import threading

import frida

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--cancel-bulk', type=int, help='cancel one pending bulk submission (1-based)')
    parser.add_argument('--hash-bulk', action='store_true', help='record hashes of successful USB chunks, never pixels')
    parser.add_argument('options', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    options = args.options[1:] if args.options[:1] == ['--'] else args.options
    if not options or options[0] != '--capture-duo' or '--stream' in options:
        parser.error('requires metadata-only --capture-duo command')
    if args.cancel_bulk is not None and args.cancel_bulk < 1:
        parser.error('--cancel-bulk must be positive')
    duration = int(options[options.index('--microseconds') + 1]) / 1e6 if '--microseconds' in options else .1
    frames = int(options[options.index('--frames') + 1]) if '--frames' in options else 1
    deadline = (duration + 45) * frames + 15
    device = frida.get_local_device()
    done = threading.Event()
    lock = threading.Lock()
    accepting = True
    with args.output.open('x', encoding='utf-8') as output:
        def record(value):
            with lock:
                if not accepting:
                    return
                output.write(json.dumps(value) + '\n')
                output.flush()

        pid = device.spawn([str(ROOT / 'target/debug/zwogain-direct.exe'), *options], stdio='pipe')
        session = device.attach(pid)
        session.on('detached', lambda *unused: done.set())
        received = 0

        def stdout(process, fd, data):
            nonlocal received
            if process == pid:
                received += len(data)
                if received > 1024 * 1024:
                    device.kill(pid)
                else:
                    record({'kind': 'worker-output', 'fd': fd, 'text': data.decode(errors='replace')})

        device.on('output', stdout)
        source = f'globalThis.TRACE_CANCEL_BULK_NUMBER = {json.dumps(args.cancel_bulk)};\n'
        source += f'globalThis.TRACE_HASH_BULK = {json.dumps(args.hash_bulk)};\n'
        script = session.create_script(source + Path(__file__).with_name('trace-transport.js').read_text())
        def message_received(message, data):
            payload = message.get('payload', message)
            if payload.get('kind') == 'bulk-hash-input':
                record({'kind': 'bulk-hash', 'sequence': payload['sequence'], 'bytes': len(data),
                        'sha256': hashlib.sha256(data).hexdigest(),
                        'interiorSha256': hashlib.sha256(data[4:-4]).hexdigest()})
            else:
                record(payload)
        script.on('message', message_received)
        script.load()
        try:
            device.resume(pid)
            if not done.wait(deadline):
                device.kill(pid)
                raise RuntimeError('trace deadline expired')
        finally:
            if not done.is_set():
                device.kill(pid)
            device.off('output', stdout)
            with lock:
                accepting = False
    print(args.output)


if __name__ == '__main__':
    main()
