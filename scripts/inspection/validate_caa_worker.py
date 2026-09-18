"""Validate production CAA segmented travel and return, without the SDK.

Requires clearance and cable slack for the requested positive travel.
On failure this stops; it never automatically unwinds an uncertain move.
"""
import argparse
import json
from pathlib import Path
import queue
import subprocess
import sys
import threading
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--worker', type=Path, default=Path('target/release/zwogain-caa.exe'))
    parser.add_argument('--travel', type=float, default=180)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if not 90 < args.travel <= 450:
        parser.error('travel must be greater than 90 and at most 450 degrees')
    with args.output.open('x', encoding='utf-8') as log:
        worker = subprocess.Popen([str(args.worker.resolve()), 'serve'], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, text=True, encoding='utf-8')
        replies = queue.Queue()
        def read():
            for line in worker.stdout:
                replies.put(line)
            replies.put(None)
        threading.Thread(target=read, daemon=True).start()
        def command(name, **values):
            request = dict(command=name, **values)
            worker.stdin.write(json.dumps(request) + '\n')
            worker.stdin.flush()
            response = replies.get(timeout=10)
            if response is None:
                raise RuntimeError('CAA worker exited')
            response = json.loads(response)
            log.write(json.dumps(dict(request=request, response=response)) + '\n')
            log.flush()
            if not response['ok']:
                raise RuntimeError(response['error'])
            return response['result']
        def check(state):
            if state['error'] or state['motion_error']:
                raise RuntimeError(str(state))
        try:
            initial = command('status')
            check(initial)
            if initial['moving']:
                raise RuntimeError('CAA is already moving')
            settings = command('settings')
            sign = -1 if settings['reverse'] else 1
            for travel in (args.travel, -args.travel):
                before = command('status')
                command('rotate-unwrapped', degrees=travel)
                deadline = time.monotonic() + 180
                while True:
                    state = command('status')
                    check(state)
                    if not state['moving']:
                        break
                    if time.monotonic() > deadline:
                        raise TimeoutError('segmented travel timed out')
                    time.sleep(.15)
                expected = (before['logical_degrees'] + sign * travel) % 360
                error = (state['logical_degrees'] - expected + 180) % 360 - 180
                if abs(error) > .1:
                    raise RuntimeError('logical position did not track physical travel')
                print(json.dumps(dict(travel=travel, final=state)), flush=True)
            # Only restore the old reference after both balanced moves succeeded.
            command('reference', degrees=initial['mechanical_degrees'])
            final = command('status')
            check(final)
            if abs(final['mechanical_degrees'] - initial['mechanical_degrees']) > .03:
                raise RuntimeError('reference restoration failed')
            print('Balanced segmented travel and reference restoration passed', flush=True)
        finally:
            try:
                if worker.poll() is None:
                    try:
                        command('stop')
                    except Exception as error:
                        print('Stop could not be confirmed: ' + str(error), file=sys.stderr)
            finally:
                try:
                    worker.stdin.close()
                except OSError:
                    pass
                try:
                    worker.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    worker.kill()
                    worker.wait()
                    raise RuntimeError('Worker did not close; motor stop is unconfirmed')


if __name__ == '__main__':
    main()
