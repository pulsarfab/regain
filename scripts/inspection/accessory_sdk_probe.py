"""Trace EFW/EAF SDK HID traffic in an owned child; raw evidence stays local."""
import argparse
import ctypes as C
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
SDK = {
    'efw': ROOT / '.reference/sdk-efw/EFW_Windows_SDK_V1.8.4/lib/x64/Release/EFW_filter.dll',
    'eaf': ROOT / '.reference/sdk-eaf/EAF_Windows_SDK_V1.8.1/lib/Windows/x64/Release/EAF_focuser.dll',
}


def child(args):
    prefix = args.device.upper()
    sdk = C.CDLL(str(args.sdk.resolve()))
    for suffix in ('SetDirection',) if args.device == 'efw' else ('SetBeep', 'SetReverse'):
        getattr(sdk, prefix+suffix).argtypes = [C.c_int, C.c_bool]
    version = getattr(sdk, prefix+'GetSDKVersion')
    version.restype = C.c_char_p

    def emit(value):
        print(json.dumps(value), flush=True)

    def call(suffix, *values, optional=False):
        name = prefix+suffix
        emit({'kind': 'sdk-call', 'name': name})
        code = getattr(sdk, name)(*values)
        emit({'kind': 'sdk-result', 'name': name, 'code': code})
        if code and not optional:
            raise RuntimeError(f'{name}: {code}')
        return code

    emit({'kind': 'ready', 'sdkVersion': version().decode(), 'sdkBase': hex(sdk._handle)})
    if sys.stdin.readline().strip() != 'start':
        raise RuntimeError('missing handshake')
    count = getattr(sdk, prefix+'GetNum')()
    emit({'kind': 'enumeration', 'count': count})
    if count != 1:
        raise RuntimeError('exactly one device of the selected type is required')
    device = C.c_int()
    call('GetID', 0, C.byref(device))
    call('Open', device)
    try:
        class Info(C.Structure):
            _fields_ = [('id', C.c_int), ('name', C.c_char*64), ('capacity', C.c_int)]
        info = Info()
        call('GetProperty', device, C.byref(info))
        emit({'kind': 'property', 'id': info.id, 'name': info.name.decode(), 'capacity': info.capacity})
        firmware = [C.c_ubyte() for _ in range(3)]
        call('GetFirmwareVersion', device, *(C.byref(x) for x in firmware))
        serial = C.create_string_buffer(8)
        code = call('GetSerialNumber', device, serial, optional=True)
        emit({'kind': 'identity', 'firmware': [x.value for x in firmware], 'serial': serial.raw.hex() if code == 0 else None})
        getters = [('Position', C.c_int)]
        if args.device == 'efw':
            getters += [('Direction', C.c_bool), ('HWErrorCode', C.c_int)]
        else:
            getters += [('MaxStep', C.c_int), ('Backlash', C.c_int), ('Reverse', C.c_bool), ('Beep', C.c_bool), ('Temp', C.c_float)]
        snapshot = {}
        for suffix, typ in getters:
            value = typ()
            code = call('Get'+suffix, device, C.byref(value), optional=suffix == 'Temp')
            snapshot[suffix] = value.value if code == 0 else None
        if args.device == 'eaf':
            moving, hand = C.c_bool(), C.c_bool()
            call('IsMoving', device, C.byref(moving), C.byref(hand))
            snapshot.update(moving=moving.value, handControl=hand.value)
            model = C.create_string_buffer(16)
            call('GetType', device, model, optional=True)
            snapshot['model'] = model.value.decode()
        emit({'kind': 'snapshot', **snapshot})
        if args.calibrate:
            start, slots = snapshot['Position'], info.capacity
            if start < 0 or snapshot['HWErrorCode'] != 0:
                raise RuntimeError('calibration requires an idle, healthy wheel')
            started = time.monotonic()
            call('Calibrate', device)
            saw_moving = False
            while time.monotonic()-started < 90:
                position, hardware_error = C.c_int(), C.c_int()
                call('GetPosition', device, C.byref(position))
                call('GetHWErrorCode', device, C.byref(hardware_error))
                if hardware_error.value:
                    raise RuntimeError(f'calibration hardware error: {hardware_error.value}')
                saw_moving |= position.value == -1
                emit({'kind': 'calibration-sample', 'position': position.value,
                      'hardwareError': hardware_error.value,
                      'elapsedSeconds': round(time.monotonic()-started, 3)})
                if saw_moving and position.value >= 0:
                    break
                time.sleep(.1)
            else:
                # There is no verified wheel halt. Do not send another motion
                # command or retry an uncertain calibration.
                raise TimeoutError('calibration did not report moving followed by idle')
            call('GetProperty', device, C.byref(info))
            if info.capacity != slots:
                raise RuntimeError(f'calibration changed slot count: {slots} -> {info.capacity}')
            emit({'kind': 'calibrated', 'elapsedSeconds': round(time.monotonic()-started, 3),
                  'position': position.value, 'slots': info.capacity, 'hardwareError': 0})
            if position.value != start:
                call('SetPosition', device, start)
                deadline = time.monotonic()+30
                while time.monotonic() < deadline:
                    call('GetPosition', device, C.byref(position))
                    if position.value == start:
                        break
                    time.sleep(.1)
                else:
                    raise TimeoutError('post-calibration position restoration timed out')
            direction = C.c_bool()
            call('GetDirection', device, C.byref(direction))
            if direction.value != snapshot['Direction']:
                raise RuntimeError('calibration changed direction setting')
            emit({'kind': 'calibration-restored', 'position': position.value,
                  'direction': direction.value, 'slots': info.capacity})
        if args.exercise:
            start = snapshot['Position']
            assert start >= 0 and not snapshot.get('moving') and not snapshot.get('handControl')
            def wait(target):
                deadline = time.monotonic()+30
                while time.monotonic() < deadline:
                    value = C.c_int()
                    call('GetPosition', device, C.byref(value))
                    moving = value.value == -1
                    if args.device == 'eaf':
                        active, hand = C.c_bool(), C.c_bool()
                        call('IsMoving', device, C.byref(active), C.byref(hand))
                        moving = active.value or hand.value
                    emit({'kind': 'motion-sample', 'position': value.value, 'moving': moving})
                    if not moving and value.value == target:
                        return
                    time.sleep(.1)
                raise TimeoutError('motion did not complete')
            move = 'SetPosition' if args.device == 'efw' else 'Move'
            restore = []
            try:
                targets = list(range(info.capacity))+[start] if args.device == 'efw' else [start+50 if start+50<=snapshot['MaxStep'] else start-50, start]
                for target in targets:
                    call(move, device, target)
                    wait(target)
                if args.device == 'efw':
                    restore = [('SetDirection', snapshot['Direction'])]
                    call('SetDirection', device, not snapshot['Direction'])
                else:
                    call('Stop', device)
                    restore = [('SetBeep', snapshot['Beep']), ('SetReverse', snapshot['Reverse']),
                               ('SetBacklash', snapshot['Backlash']), ('SetMaxStep', snapshot['MaxStep'])]
                    for suffix, value in [('SetBeep', not snapshot['Beep']), ('SetReverse', not snapshot['Reverse']),
                                          ('SetBacklash', 1 if snapshot['Backlash']==0 else 0), ('SetMaxStep', snapshot['MaxStep'])]:
                        call(suffix, device, value)
            finally:
                if args.device == 'eaf':
                    call('Stop', device)
                for suffix, value in restore:
                    call(suffix, device, value)
                call(move, device, start)
                wait(start)
            emit({'kind': 'restored', 'position': start, 'settings': dict(restore)})
        emit({'kind': 'complete'})
    finally:
        call('Close', device)


HOOK = r'''
for (const [name, offset] of Object.entries(IMPORTS)) {
  Interceptor.attach(ptr(SDK_BASE).add(offset).readPointer(), {
    onEnter(args) {
      this.buffer=args[1]; this.length=args[2].toUInt32();
      if (this.length>1024) throw new Error('oversized HID report');
      this.before=Array.from(new Uint8Array(this.buffer.readByteArray(this.length)));
    },
    onLeave(result) {
      send({kind:'hid',name,length:this.length,ok:result.toInt32()!==0,
        bytes:name.includes('Set')?this.before:Array.from(new Uint8Array(this.buffer.readByteArray(this.length)))});
    }
  });
}
send({kind:'hooks-ready'});
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('device', choices=SDK)
    parser.add_argument('--sdk', type=Path)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--exercise', action='store_true', help='EFW slot sweep or EAF 50-step round trip; restore settings')
    parser.add_argument('--calibrate', action='store_true', help='EFW only: calibrate once, verify completion and slot count, restore position')
    parser.add_argument('--child', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.calibrate and args.device != 'efw':
        parser.error('--calibrate requires efw')
    args.sdk = args.sdk or SDK[args.device]
    if args.child:
        child(args)
        return
    import frida
    import pefile
    if args.output is None:
        parser.error('--output required')
    pe = pefile.PE(str(args.sdk))
    names = {'HidD_SetOutputReport', 'HidD_GetInputReport', 'HidD_SetFeature', 'HidD_GetFeature'}
    imports = {i.name.decode(): i.address-pe.OPTIONAL_HEADER.ImageBase
               for d in pe.DIRECTORY_ENTRY_IMPORT for i in d.imports
               if i.name and i.name.decode() in names}
    if not imports:
        raise RuntimeError('no imported HID transport found')
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open('x', encoding='utf-8') as log:
        lock, ready, observations = threading.Lock(), threading.Event(), []
        def record(value):
            with lock:
                observations.append(value)
                log.write(json.dumps({'time': time.time(), **value})+'\n')
                log.flush()
        record({'kind': 'configuration', 'device': args.device,
                'sdkSha256': hashlib.sha256(args.sdk.read_bytes()).hexdigest(), 'imports': imports})
        command = [sys._base_executable, str(Path(__file__).resolve()), args.device, '--child', '--sdk', str(args.sdk)]
        if args.exercise:
            command.append('--exercise')
        if args.calibrate:
            command.append('--calibrate')
        proc = subprocess.Popen(command, stdin=subprocess.PIPE,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        session = None
        timer = threading.Timer(180 if args.calibrate else 90, proc.kill)
        timer.start()
        try:
            initial = json.loads(proc.stdout.readline())
            record(initial)
            session = frida.attach(proc.pid)
            script = session.create_script('const SDK_BASE='+json.dumps(initial['sdkBase'])+
                                           '; const IMPORTS='+json.dumps(imports)+';\n'+HOOK)
            def receive(message, data):
                value = message.get('payload', {'kind': 'trace-error', 'message': message})
                record(value)
                if value.get('kind') == 'hooks-ready':
                    ready.set()
            script.on('message', receive)
            script.load()
            if not ready.wait(5):
                raise RuntimeError('HID hooks failed')
            proc.stdin.write('start\n')
            proc.stdin.flush()
            for line in proc.stdout:
                record(json.loads(line))
            code = proc.wait()
            errors = proc.stderr.read()
            record({'kind': 'process-exit', 'code': code, 'stderr': errors})
            if code or any(v['kind'] == 'trace-error' for v in observations):
                raise RuntimeError(errors or 'trace failed')
            if not any(v['kind'] == 'hid' for v in observations):
                raise RuntimeError('no HID traffic captured')
        finally:
            timer.cancel()
            if proc.poll() is None:
                proc.kill()
                proc.wait()
            if session:
                try:
                    session.detach()
                except frida.InvalidOperationError:
                    pass
    print(args.output)


if __name__ == '__main__':
    main()
