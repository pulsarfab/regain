"""Bounded ASI585MM Pro/ASI662MC/ASI676MC SDK video reference; pixels discarded.

Manual hardware research only, never called by CI. Owns a disposable child and
traces only that child. Raw USB traces contain device paths: keep output ignored.
One attached ASI is required unless discovery of other cameras is authorized.
"""
import argparse
import ctypes as c
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
SDK = ROOT / 'vendor/zwo/ASICamera2.dll'
SDK_SHA = '0c8778c3cce2012961b079e3c7d0d8348a8b3823939335d9e98148cb5d5dc34a'


class CameraInfo(c.Structure):
    _fields_ = [('name', c.c_char * 64), ('id', c.c_int),
                ('height', c.c_long), ('width', c.c_long),
                ('color', c.c_int), ('bayer', c.c_int),
                ('bins', c.c_int * 16), ('formats', c.c_int * 8),
                ('pixel_size', c.c_double), ('shutter', c.c_int),
                ('st4', c.c_int), ('cooled', c.c_int), ('usb3host', c.c_int),
                ('usb3camera', c.c_int), ('electrons', c.c_float),
                ('depth', c.c_int), ('trigger', c.c_int), ('unused', c.c_char * 16)]


def worker(args):
    # Attach instrumentation before loading the SDK or touching USB.
    if sys.stdin.readline().strip() != 'go':
        raise RuntimeError('missing parent handshake')
    sdk = c.CDLL(str(SDK))
    signatures = {
        'ASIGetNumOfConnectedCameras': [],
        'ASIGetCameraProperty': [c.POINTER(CameraInfo), c.c_int],
        'ASIOpenCamera': [c.c_int], 'ASIInitCamera': [c.c_int],
        'ASICloseCamera': [c.c_int], 'ASIDisableDarkSubtract': [c.c_int],
        'ASISetROIFormat': [c.c_int] * 5, 'ASISetStartPos': [c.c_int] * 3,
        'ASISetControlValue': [c.c_int, c.c_int, c.c_long, c.c_int],
        'ASIStartVideoCapture': [c.c_int], 'ASIStopVideoCapture': [c.c_int],
        'ASIGetVideoData': [c.c_int, c.POINTER(c.c_ubyte), c.c_long, c.c_int],
    }
    for name, types in signatures.items():
        getattr(sdk, name).argtypes = types
        getattr(sdk, name).restype = c.c_int

    def call(name, *params):
        code = getattr(sdk, name)(*params)
        if code != 0:
            raise RuntimeError(f'{name}: ASI error {code}')

    # GetCameraProperty may open devices; count first and refuse multiple.
    count = sdk.ASIGetNumOfConnectedCameras()
    if not 1 <= count <= 128 or (count != 1 and not args.allow_discovery):
        raise RuntimeError('reference trace requires exactly one attached ASI')
    matches = []
    for index in range(count):
        candidate = CameraInfo()
        call('ASIGetCameraProperty', c.byref(candidate), index)
        if candidate.name == args.camera_name.encode():
            matches.append(candidate)
    if len(matches) != 1:
        raise RuntimeError('exactly one selected model must match')
    info = matches[0]
    try:
        if info.name != args.camera_name.encode():
            raise RuntimeError('reference trace model mismatch')
        print(json.dumps({'kind': 'camera', 'name': info.name.decode(),
                          'usb3Host': bool(info.usb3host)}), flush=True)
        call('ASIOpenCamera', info.id)
        call('ASIInitCamera', info.id)
        call('ASIDisableDarkSubtract', info.id)
        call('ASISetStartPos', info.id, 0, 0)
        call('ASISetROIFormat', info.id, args.width, args.height, 1, 2)
        for control, value in [(0, args.gain), (1, args.microseconds),
                               (5, 15 if args.camera_name == 'ZWO ASI662MC' else 10), (6, 40), (9, 0)]:
            call('ASISetControlValue', info.id, control, value, 0)
        data = (c.c_ubyte * (args.width * args.height * 2))()
        call('ASIStartVideoCapture', info.id)
        try:
            for frame in range(args.frames):
                start = time.monotonic()
                call('ASIGetVideoData', info.id, data, len(data),
                     args.microseconds // 1000 * 2 + 5000)
                print(json.dumps({'kind': 'video-frame', 'frame': frame,
                                  'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest(),
                                  'elapsedMs': (time.monotonic() - start) * 1000}), flush=True)
        finally:
            call('ASIStopVideoCapture', info.id)
    finally:
        call('ASICloseCamera', info.id)
    print(json.dumps({'kind': 'experiment-complete'}), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--frames', type=int, default=8)
    parser.add_argument('--camera-name', choices=['ZWO ASI662MC', 'ZWO ASI676MC', 'ZWO ASI585MM Pro'], default='ZWO ASI662MC')
    parser.add_argument('--width', type=int)
    parser.add_argument('--height', type=int)
    parser.add_argument('--microseconds', type=int, default=100000)
    parser.add_argument('--gain', type=int, default=0)
    parser.add_argument('--deadline', type=int, default=90)
    parser.add_argument('--allow-discovery', action='store_true',
                        help='operator-approved one-time SDK discovery with other cameras attached')
    parser.add_argument('--worker', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    full_width, full_height = {'ZWO ASI662MC': (1920, 1080), 'ZWO ASI676MC': (3552, 3552), 'ZWO ASI585MM Pro': (3840, 2160)}[args.camera_name]
    args.width = full_width if args.width is None else args.width
    args.height = full_height if args.height is None else args.height
    if not (1 <= args.frames <= 100 and 32 <= args.microseconds <= 30000000
            and 64 <= args.width <= full_width and args.width % 8 == 0
            and 64 <= args.height <= full_height and args.height % 2 == 0
            and 0 <= args.gain <= 600 and 1 <= args.deadline <= 300):
        parser.error('invalid bounded video parameters')
    if sys.platform != 'win32' or hashlib.sha256(SDK.read_bytes()).hexdigest() != SDK_SHA:
        parser.error('requires the pinned Windows SDK 1.41')
    if args.worker:
        worker(args)
        return
    import frida
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open('x', encoding='utf-8') as log:
        lock = threading.Lock()
        failed = threading.Event()
        sdk_seen = threading.Event()
        video_seen = threading.Event()

        def record(value):
            with lock:
                log.write(json.dumps(value) + '\n')
                log.flush()

        record({'kind': 'configuration', 'sdkSha256': SDK_SHA,
                'parameters': {k: str(v) if isinstance(v, Path) else v for k, v in vars(args).items()}})
        # Windows venv python.exe is a redirector; attach to the real worker.
        proc = subprocess.Popen([sys._base_executable, __file__, *sys.argv[1:], '--worker'],
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, text=True)
        timer = threading.Timer(args.deadline, proc.kill)
        timer.start()
        session = None
        try:
            session = frida.attach(proc.pid)
            script = session.create_script((ROOT / 'scripts/inspection/trace-transport.js').read_text())

            def message(value, data):
                if value['type'] == 'send':
                    payload = value['payload']
                    if payload.get('kind') == 'sdk-module':
                        sdk_seen.set()
                    if payload.get('kind') == 'sdk-leave' and payload.get('name') == 'ASIGetVideoData':
                        video_seen.set()
                    record(payload)
                else:
                    failed.set()
                    record({'kind': 'instrumentation-error', 'message': value})

            script.on('message', message)
            script.load()
            proc.stdin.write('go\n')
            proc.stdin.flush()
            complete = False
            for line in proc.stdout:
                try:
                    value = json.loads(line)
                    complete |= value.get('kind') == 'experiment-complete'
                    record(value)
                except ValueError:
                    record({'kind': 'worker-output', 'text': line.rstrip()})
            if (proc.wait() != 0 or not complete or failed.is_set()
                    or not sdk_seen.is_set() or not video_seen.is_set()):
                raise RuntimeError('video experiment failed; inspect local trace')
        finally:
            if proc.poll() is None:
                proc.kill()
            proc.wait(timeout=5)
            timer.cancel()
            timer.join()
            if session:
                try:
                    session.detach()
                except frida.InvalidOperationError:
                    pass
    print(args.output)


if __name__ == '__main__':
    main()
