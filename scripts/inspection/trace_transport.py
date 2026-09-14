"""Trace an owned Rust SDK host during bounded captures; discard all image bytes."""
import argparse
import array
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import threading
import time

import frida

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--frames', type=int, default=1)
    parser.add_argument('--seconds', type=float, default=0.1)
    parser.add_argument('--width', type=int, default=512)
    parser.add_argument('--height', type=int, default=256)
    parser.add_argument('--bin', type=int, default=1)
    parser.add_argument('--x', type=int, default=0, help='binned ROI origin')
    parser.add_argument('--y', type=int, default=0, help='binned ROI origin')
    parser.add_argument('--ready-delay', type=float, default=0.5)
    parser.add_argument('--deadline', type=float, default=60)
    parser.add_argument('--cancel-first-bulk', action='store_true',
                        help='fault experiment: cancel one pending USB bulk request in this host')
    parser.add_argument('--hash-bulk', action='store_true',
                        help='hash completed bulk buffers locally; changes timing, never saves pixel data')
    parser.add_argument('--compare-wire', action='store_true',
                        help='compare a complete USB pass to SDK RAW16 in memory; saves only statistics')
    parser.add_argument('--trace-processing', action='store_true', help='observe version-pinned SDK retrieval target')
    args = parser.parse_args()
    if not (1 <= args.frames <= 20 and 0 < args.seconds <= 30 and 0 <= args.ready_delay <= 5
            and 0 < args.deadline <= 300 and args.width > 0 and args.height > 0
            and args.width % 8 == 0 and args.height % 2 == 0
            and args.width * args.height * 2 <= 512 * 1024 * 1024):
        parser.error('invalid bounded capture parameters')
    if args.bin not in [1, 2, 4] or args.x < 0 or args.y < 0:
        parser.error('invalid binning/ROI origin')
    if args.trace_processing and not args.compare_wire:
        parser.error('--trace-processing requires --compare-wire')
    if args.compare_wire and args.width * args.height * 2 > 128 * 1024 * 1024:
        parser.error('wire comparison is limited to 128 MiB output frames')
    args.output.parent.mkdir(parents=True, exist_ok=True)
    sdk = ROOT / 'vendor/zwo/ASICamera2.dll'
    if args.trace_processing and hashlib.sha256(sdk.read_bytes()).hexdigest() != '0c8778c3cce2012961b079e3c7d0d8348a8b3823939335d9e98148cb5d5dc34a':
        parser.error('processing hooks require the inspected SDK 1.41 x64 binary')
    host = ROOT / 'target/debug/zwogain-host.exe'
    with args.output.open('x', encoding='utf-8') as log:
        lock = threading.Lock()

        def record(value):
            with lock:
                log.write(json.dumps(value) + '\n')
                log.flush()

        record({'kind': 'configuration', 'timeMs': time.time_ns() // 1_000_000,
                'parameters': {k: str(v) if isinstance(v, Path) else v for k, v in vars(args).items()},
                'sdkSha256': hashlib.sha256(sdk.read_bytes()).hexdigest(), 'frida': frida.__version__})
        proc = subprocess.Popen([str(host), '--sdk', str(sdk)], stdin=subprocess.PIPE,
                                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        expired = threading.Event()
        instrumentation_error = threading.Event()
        wire_lock = threading.Lock()
        wire_chunks = {}
        wire_submits = []
        processing = {}
        wire_bytes = 0
        download_seen = threading.Event()

        def timeout():
            expired.set()
            try:
                proc.kill()
            except OSError:
                pass

        watchdog = threading.Timer(args.deadline, timeout)
        watchdog.start()
        session = None
        script = None
        try:
            session = frida.attach(proc.pid)
            source = f'globalThis.TRACE_CANCEL_FIRST_BULK = {json.dumps(args.cancel_first_bulk)};\n'
            source += f'globalThis.TRACE_HASH_BULK = {json.dumps(args.hash_bulk or args.compare_wire)};\n'
            source += f'globalThis.TRACE_PROCESSING = {json.dumps(args.trace_processing)};\n'
            source += Path(__file__).with_name('trace-transport.js').read_text()
            script = session.create_script(source)

            def message_received(message, data):
                nonlocal wire_bytes
                value = message.get('payload', {})
                if message['type'] == 'send' and value.get('kind') == 'bulk-hash-input':
                    if args.compare_wire:
                        with wire_lock:
                            if wire_bytes + len(data) > 256 * 1024 * 1024:
                                instrumentation_error.set()
                            else:
                                wire_chunks[value['sequence']] = bytes(data)
                                wire_bytes += len(data)
                    swapped_hash = None
                    if len(data) % 2 == 0:
                        swapped = array.array('H')
                        swapped.frombytes(data)
                        swapped.byteswap()
                        swapped_hash = hashlib.sha256(swapped).hexdigest()
                    record({'kind': 'bulk-hash', 'sequence': value['sequence'], 'bytes': len(data),
                            'sha256': hashlib.sha256(data).hexdigest(),
                            'sha256Swap16': swapped_hash})
                elif message['type'] == 'send' and value.get('kind') == 'processing-buffer':
                    with wire_lock:
                        processing[value['stage']] = bytes(data)
                elif message['type'] == 'send':
                    record(value)
                    if value.get('kind') == 'io-submit' and value.get('code') == '0x22004b':
                        with wire_lock:
                            wire_submits.append(value['sequence'])
                    if value.get('kind') == 'sdk-leave' and value.get('name') == 'ASIGetDataAfterExp':
                        download_seen.set()
                else:
                    instrumentation_error.set()
                    record({'kind': 'instrumentation-error', 'message': message})

            script.on('message', message_received)
            script.load()
            next_id = 0

            def read_exact(size):
                chunks = bytearray()
                while len(chunks) < size:
                    data = proc.stdout.read(size - len(chunks))
                    if not data:
                        raise RuntimeError('host deadline expired' if expired.is_set() else 'host exited')
                    chunks.extend(data)
                return chunks

            def call(method, params=None):
                nonlocal next_id
                next_id += 1
                body = json.dumps({'version': 1, 'id': next_id, 'method': method, 'params': params}).encode()
                record({'kind': 'command', 'name': method, 'timeMs': time.time_ns() // 1_000_000})
                proc.stdin.write(struct.pack('<I', len(body)) + body)
                proc.stdin.flush()
                length, = struct.unpack('<I', read_exact(4))
                if not 0 < length <= 65536:
                    raise RuntimeError('invalid protocol length')
                reply = json.loads(read_exact(length))
                if reply['id'] != next_id or reply['version'] != 1 or not reply['ok']:
                    raise RuntimeError(reply)
                size = reply['binaryLength']
                if not 0 <= size <= 512 * 1024 * 1024:
                    raise RuntimeError('invalid frame length')
                digest = hashlib.sha256()
                chunk_hashes = []
                remaining = size
                pixels = bytearray()
                while remaining:
                    chunk = read_exact(min(remaining, 1024 * 1024))
                    digest.update(chunk)
                    if args.compare_wire:
                        pixels.extend(chunk)
                    if args.hash_bulk:
                        chunk_hashes.append(hashlib.sha256(chunk).hexdigest())
                    remaining -= len(chunk)
                if size:
                    record({'kind': 'frame', 'timeMs': time.time_ns() // 1_000_000,
                            'bytes': size, 'sha256': digest.hexdigest(), 'chunkSha256': chunk_hashes})
                    if args.compare_wire:
                        if not download_seen.wait(5):
                            raise RuntimeError('missing instrumentation download completion')
                        from compare_wire import compare, complete_runs
                        with wire_lock:
                            runs, rejected = complete_runs(wire_submits, wire_chunks, size)
                        record({'kind': 'wire-runs', 'completeRuns': len(runs), 'rejectedRunBytes': rejected})
                        if len(runs) > 1:
                            record({'kind': 'wire-replay-identity', 'completeRuns': len(runs),
                                    'allBytesIdentical': all(wire == runs[0] for wire in runs[1:]),
                                    'bytesPerRun': size})
                        for index, wire in enumerate(runs):
                            record({'kind': 'wire-comparison', 'run': index,
                                    **compare(wire, pixels, args.width, args.height)})
                        for stage, data in processing.items():
                            record({'kind': 'processing-comparison', 'stage': stage,
                                    **compare(data, pixels, args.width, args.height)})
                return reply['result']

            cameras = call('list')
            if len(cameras) != 1:
                raise RuntimeError('exactly one attached ASI camera is required')
            camera = cameras[0]
            record({'kind': 'camera', 'name': camera['name'], 'width': camera['width'], 'height': camera['height']})
            if ((args.x + args.width) * args.bin > camera['width']
                    or (args.y + args.height) * args.bin > camera['height']
                    or args.bin not in camera['bins']):
                raise RuntimeError('ROI exceeds attached sensor')
            opened = call('open', {'name': camera['name']})
            record({'kind': 'initial-controls', 'sdkVersion': opened['sdkVersion'], 'controls': opened['controls']})
            for _ in range(args.frames):
                with wire_lock:
                    wire_chunks.clear()
                    wire_submits.clear()
                    processing.clear()
                    wire_bytes = 0
                download_seen.clear()
                call('start', {'width': args.width, 'height': args.height, 'bin': args.bin, 'x': args.x, 'y': args.y,
                               'microseconds': int(args.seconds * 1_000_000), 'dark': False})
                while True:
                    status = call('status')
                    if status == 2:
                        break
                    if status != 1:
                        record({'kind': 'exposure-failed', 'status': status, 'timeMs': time.time_ns() // 1_000_000})
                        if args.cancel_first_bulk and status == 3:
                            break
                        raise RuntimeError(f'exposure state {status}')
                    time.sleep(0.02)
                if status != 2:
                    call('stop')
                    break
                time.sleep(args.ready_delay)
                call('download')
                record({'kind': 'post-download-status', 'status': call('status')})
            call('close')
            if instrumentation_error.is_set():
                raise RuntimeError('instrumentation failed; inspect trace errors')
            record({'kind': 'experiment-complete'})
        finally:
            try:
                if proc.poll() is None:
                    proc.stdin.close()
                    try:
                        proc.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        proc.kill()
                        proc.wait(timeout=5)
            finally:
                watchdog.cancel()
                watchdog.join()
                if session:
                    try:
                        session.detach()
                    except frida.InvalidOperationError:
                        pass
    print(args.output)


if __name__ == '__main__':
    main()
