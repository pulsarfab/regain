"""Trace one owned SDK-free worker, with bounded metadata-only stdout."""
import argparse
import json
from pathlib import Path
import threading

import frida

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('options', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    options = args.options[1:] if args.options[:1] == ['--'] else args.options
    if not options or options[0] != '--capture-duo' or '--stream' in options:
        parser.error('requires metadata-only --capture-duo command')
    device = frida.get_local_device()
    done = threading.Event()
    lock = threading.Lock()
    with args.output.open('x', encoding='utf-8') as output:
        def record(value):
            with lock:
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
        script = session.create_script(Path(__file__).with_name('trace-transport.js').read_text())
        script.on('message', lambda message, data: record(message.get('payload', message)))
        script.load()
        try:
            device.resume(pid)
            if not done.wait(120):
                device.kill(pid)
                raise RuntimeError('trace deadline expired')
        finally:
            if not done.is_set():
                device.kill(pid)
            device.off('output', stdout)
    print(args.output)


if __name__ == '__main__':
    main()
