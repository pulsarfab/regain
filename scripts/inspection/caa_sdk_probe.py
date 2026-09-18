"""Trace CAA SDK HID reports in an owned process. Private output stays local."""
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
DEFAULT_SDK = ROOT / 'CAA_Windows_SDK_V1.5.9/lib/x64/Release/CAA_SRC.dll'

def child(sdk_path, exercise):
    sdk = C.CDLL(str(sdk_path.resolve()))
    sdk.CAAGetSDKVersion.restype = C.c_char_p
    for name in ['CAAMove', 'CAAMoveTo', 'CAAMoveToMechanical', 'CAACurDegree', 'CAASetMaxDegree']:
        getattr(sdk, name).argtypes = [C.c_int, C.c_float]
    for name in ['CAASetBeep', 'CAASetReverse']:
        getattr(sdk, name).argtypes = [C.c_int, C.c_bool]

    def emit(value):
        print(json.dumps(value), flush=True)

    def call(name, *args):
        emit({'kind': 'sdk-call', 'name': name})
        code = getattr(sdk, name)(*args)
        emit({'kind': 'sdk-result', 'name': name, 'code': code})
        if code != 0 and not (name == 'CAAGetTemp' and code in (7, 8)):
            raise RuntimeError(f'{name}: {code}')
        return code

    def snapshot(device):
        result = {}
        for name, kind in [('Degree', C.c_float), ('MaxDegree', C.c_float), ('Temp', C.c_float), ('Beep', C.c_bool), ('Reverse', C.c_bool)]:
            v = kind()
            code = call('CAAGet'+name, device, C.byref(v))
            result[name] = v.value if code == 0 else None
            if code: result[name+'Error'] = code
        moving, hand = C.c_bool(), C.c_bool()
        call('CAAIsMoving', device, C.byref(moving), C.byref(hand))
        result.update(moving=moving.value, handControl=hand.value)
        emit({'kind': 'snapshot', **result})
        return result

    def wait(device, target):
        deadline = time.monotonic()+20
        while time.monotonic() < deadline:
            moving, hand, angle = C.c_bool(), C.c_bool(), C.c_float()
            call('CAAIsMoving', device, C.byref(moving), C.byref(hand))
            call('CAAGetDegree', device, C.byref(angle))
            emit({'kind':'motion-sample','moving':moving.value,'angle':angle.value})
            if not moving.value and abs(angle.value-target) <= .1:
                return
            time.sleep(.1)
        raise TimeoutError('CAA did not reach target')

    emit({'kind':'ready','sdkVersion':sdk.CAAGetSDKVersion().decode(),'sdkBase':hex(sdk._handle)})
    if sys.stdin.readline().strip() != 'start':
        raise RuntimeError('missing parent handshake')
    count = sdk.CAAGetNum()
    emit({'kind':'enumeration','count':count})
    if count != 1:
        raise RuntimeError('exactly one CAA is required for this workup')
    device = C.c_int()
    call('CAAGetID', 0, C.byref(device))
    call('CAAOpen', device)
    initial = None
    try:
        initial = snapshot(device)
        assert not initial['moving'] and not initial['handControl']
        firmware = [C.c_ubyte() for _ in range(3)]
        call('CAAGetFirmwareVersion', device, *(C.byref(x) for x in firmware))
        model, serial = C.create_string_buffer(16), C.create_string_buffer(8)
        call('CAAGetType', device, model)
        call('CAAGetSerialNumber', device, serial)
        emit({'kind':'identity','firmware':[x.value for x in firmware], 'model':model.value.decode(), 'serial':serial.raw.hex()})
        if exercise:
            start, maximum = initial['Degree'], initial['MaxDegree']
            target = start + 2 if start + 2 <= maximum else start - 2
            assert 0 <= target <= maximum
            call('CAAMoveTo', device, target)
            wait(device, target)
            call('CAAMoveTo', device, start)
            wait(device, start)
            call('CAASetBeep', device, not initial['Beep'])
            assert snapshot(device)['Beep'] != initial['Beep']
            call('CAASetBeep', device, initial['Beep'])
            call('CAASetReverse', device, not initial['Reverse'])
            assert snapshot(device)['Reverse'] != initial['Reverse']
            call('CAASetReverse', device, initial['Reverse'])
            call('CAASetMaxDegree', device, maximum)
            final = snapshot(device)
            assert abs(final['Degree']-start) <= .1
            for key in ['Beep','Reverse','MaxDegree']:
                assert final[key] == initial[key]
        emit({'kind':'complete','exercise':exercise})
    finally:
        if exercise and initial is not None:
            # Stop before any restoration. Each cleanup operation is attempted
            # separately so one failed setting cannot prevent position restore.
            cleanup = [('CAAStop',()), ('CAASetBeep',(initial['Beep'],)), ('CAASetReverse',(initial['Reverse'],)), ('CAAMoveTo',(initial['Degree'],))]
            for name,args in cleanup:
                try: call(name,device,*args)
                except Exception as error: emit({'kind':'cleanup-error','error':str(error)})
            try: wait(device,initial['Degree'])
            except Exception as error: emit({'kind':'cleanup-error','error':str(error)})
        call('CAAClose',device)

HOOK = r'''
for (const name of ['HidD_SetOutputReport','HidD_GetInputReport']) {
  const address = ptr(SDK_BASE).add(name==='HidD_SetOutputReport'?0xa048:0xa040).readPointer();
  Interceptor.attach(address, {
    onEnter(args) {
      this.buffer=args[1]; this.length=args[2].toUInt32();
      if (this.length>128) throw new Error('oversized HID report');
      this.before=Array.from(new Uint8Array(this.buffer.readByteArray(this.length)));
    },
    onLeave(result) {
      send({kind:'hid',name,length:this.length,ok:result.toInt32()!==0,
        bytes:name==='HidD_SetOutputReport'?this.before:Array.from(new Uint8Array(this.buffer.readByteArray(this.length)))});
    }
  });
}
send({kind:'hooks-ready'});
'''

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sdk',type=Path,default=DEFAULT_SDK)
    parser.add_argument('--output',type=Path)
    parser.add_argument('--exercise',action='store_true',help='move two degrees and restore position/settings')
    parser.add_argument('--child',action='store_true',help=argparse.SUPPRESS)
    args=parser.parse_args()
    if args.child:
        child(args.sdk,args.exercise)
        return
    import frida
    if args.output is None: parser.error('--output required')
    args.output.parent.mkdir(parents=True,exist_ok=True)
    with args.output.open('x',encoding='utf-8') as log:
        lock=threading.Lock()
        observations=[]
        def record(value):
            with lock:
                observations.append(value)
                log.write(json.dumps({'time':time.time(),**value})+'\n'); log.flush()
        record({'kind':'configuration','sdkSha256':hashlib.sha256(args.sdk.read_bytes()).hexdigest(),'exercise':args.exercise})
        # A Windows venv python.exe may be a redirector process. Attach to the
        # actual interpreter that owns the DLL, not that intermediate launcher.
        command=[sys._base_executable,str(Path(__file__).resolve()),'--child','--sdk',str(args.sdk)]
        if args.exercise: command.append('--exercise')
        proc=subprocess.Popen(command,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        session=None
        timer=threading.Timer(90,proc.kill); timer.start()
        try:
            child_ready=json.loads(proc.stdout.readline())
            record(child_ready)
            session=frida.attach(proc.pid)
            if hashlib.sha256(args.sdk.read_bytes()).hexdigest() != '413d629adfd81150962211d2241aec2a99420ba2d65c0161f7700da32d534152':
                raise RuntimeError('HID import hooks require inspected SDK 1.5.9 x64')
            script=session.create_script('const SDK_BASE='+json.dumps(child_ready['sdkBase'])+';\n'+HOOK)
            ready=threading.Event()
            def receive(message,data):
                value=message.get('payload',{'kind':'trace-error','message':message})
                record(value)
                if value.get('kind')=='hooks-ready': ready.set()
            script.on('message',receive)
            script.load()
            if not ready.wait(5): raise RuntimeError('HID hooks failed to initialize')
            proc.stdin.write('start\n'); proc.stdin.flush()
            for line in proc.stdout: record(json.loads(line))
            code=proc.wait()
            errors=proc.stderr.read()
            record({'kind':'process-exit','code':code,'stderr':errors})
            if code: raise RuntimeError(errors)
            with lock:
                if any(v.get('kind') in ('trace-error','cleanup-error') for v in observations):
                    raise RuntimeError('CAA tracing or state restoration failed; inspect the log')
                if not any(v.get('kind')=='hid' for v in observations):
                    raise RuntimeError('no CAA HID reports captured')
        finally:
            timer.cancel()
            if proc.poll() is None: proc.kill(); proc.wait()
            if session is not None:
                try: session.detach()
                except frida.InvalidOperationError: pass
    print(args.output)

if __name__=='__main__': main()
