"""Bounded, owned QHY SDK reference probe. Full frames only; no image payload in traces."""
import argparse
import ctypes as c
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time


def probe(args):
    input()  # Parent attaches passive tracing before the SDK is loaded.
    directory = os.add_dll_directory(str(args.sdk.parent))
    sdk = c.WinDLL(str(args.sdk))
    u, p, d = c.c_uint32, c.c_void_p, c.c_double
    def bind(name, restype, types):
        fn = getattr(sdk, name)
        fn.restype, fn.argtypes = restype, types
        return fn
    def call(name, types=(), values=(), restype=u, check=True):
        start = time.perf_counter()
        result = bind(name, restype, types)(*values)
        print(json.dumps({'api': name, 'result': result, 'elapsedMs': (time.perf_counter()-start)*1000}), flush=True)
        if check and result not in (0,):
            raise RuntimeError(f'{name}: {result:#x}')
        return result
    call('EnableQHYCCDMessage', [c.c_bool], [False], restype=None, check=False)
    call('EnableQHYCCDLogFile', [c.c_bool], [False], restype=None, check=False)
    call('InitQHYCCDResource')
    handle = None
    try:
        count = call('ScanQHYCCD', check=False)
        if not 0 < count < 32:
            raise RuntimeError(f'Expected attached QHY camera, got {count}')
        cameras = []
        for index in range(count):
            identity, model = c.create_string_buffer(256), c.create_string_buffer(256)
            call('GetQHYCCDId', [u, p], [index, identity])
            call('GetQHYCCDModel', [p, p], [identity, model])
            cameras.append((identity.value, model.value.decode()))
        print(json.dumps({'cameras': [(i.decode(), m) for i,m in cameras]}), flush=True)
        selected = [(i,m) for i,m in cameras if m.startswith('QHY600')]
        if len(selected) != 1:
            raise RuntimeError('Select exactly one attached QHY600')
        identity, model = selected[0]
        handle = call('OpenQHYCCD', [c.c_char_p], [identity], restype=p, check=False)
        if not handle:
            raise RuntimeError('OpenQHYCCD returned null')
        modes = u()
        call('GetQHYCCDNumberOfReadModes', [p,p], [handle,c.byref(modes)])
        for mode in range(modes.value):
            name = c.create_string_buffer(256)
            w,h = u(),u()
            call('GetQHYCCDReadModeName', [p,u,p], [handle,mode,name])
            call('GetQHYCCDReadModeResolution', [p,u,p,p], [handle,mode,c.byref(w),c.byref(h)])
            print(json.dumps({'readMode':mode,'name':name.value.decode(),'width':w.value,'height':h.value}), flush=True)
        call('SetQHYCCDReadMode', [p,u], [handle,args.mode])
        call('SetQHYCCDStreamMode', [p,c.c_uint8], [handle,0])
        call('InitQHYCCD', [p], [handle])
        call('SetQHYCCDBitsMode', [p,u], [handle,16])
        cw,ch,pw,ph=d(),d(),d(),d()
        w,h,bits=u(),u(),u()
        call('GetQHYCCDChipInfo',[p,p,p,p,p,p,p,p],[handle,c.byref(cw),c.byref(ch),c.byref(w),c.byref(h),c.byref(pw),c.byref(ph),c.byref(bits)])
        print(json.dumps({'chip': {'width':w.value,'height':h.value,'pixelX':pw.value,'pixelY':ph.value,'bits':bits.value}}), flush=True)
        for api in ['GetQHYCCDEffectiveArea','GetQHYCCDOverScanArea']:
            coords = [u() for _ in range(4)]
            call(api,[p,p,p,p,p],[handle]+[c.byref(v) for v in coords])
            print(json.dumps({api:[v.value for v in coords]}),flush=True)
        for control in [6,7,8,9,10,12,14,15,16,18,20,21,22,23,24,25,34,35,57,58,59,62,63,86]:
            available=call('IsQHYCCDControlAvailable',[p,u],[handle,control],check=False)
            if available == 0 and control in [6,7,8,9,10,12,14,15,16,18]:
                lo,hi,step=d(),d(),d()
                rc=call('GetQHYCCDParamMinMaxStep',[p,u,p,p,p],[handle,control,c.byref(lo),c.byref(hi),c.byref(step)],check=False)
                value=call('GetQHYCCDParam',[p,u],[handle,control],restype=d,check=False)
                print(json.dumps({'control':control,'min':lo.value,'max':hi.value,'step':step.value,'value':value,'rangeResult':rc}),flush=True)
        if args.map_controls:
            for control,values in [(6,[0,1,25,26,27,30,50,56,60,100,199,200]),(7,[0,1,30,100,255]),(12,[0,10,50,60])]:
                saved=call('GetQHYCCDParam',[p,u],[handle,control],restype=d,check=False)
                try:
                    for value in values:
                        print(json.dumps({'setting':control,'value':value}),flush=True)
                        call('SetQHYCCDParam',[p,u,d],[handle,control,value])
                        call('GetQHYCCDParam',[p,u],[handle,control],restype=d,check=False)
                finally:
                    call('SetQHYCCDParam',[p,u,d],[handle,control,saved])
        if args.thermal:
            # Modest manual output only; always command off before leaving.
            try:
                for power in [0,16,38,0]:
                    call('SetQHYCCDParam',[p,u,d],[handle,16,power])
                    for _ in range(3):
                        time.sleep(1)
                        call('GetQHYCCDParam',[p,u],[handle,14],restype=d,check=False)
                        call('GetQHYCCDParam',[p,u],[handle,15],restype=d,check=False)
            finally:
                call('SetQHYCCDParam',[p,u,d],[handle,16,0])
        if args.auto_cooler:
            try:
                for _ in range(30):
                    call('ControlQHYCCDTemp',[p,d],[handle,-5.0])
                    time.sleep(1)
                    call('GetQHYCCDParam',[p,u],[handle,14],restype=d,check=False)
                    call('GetQHYCCDParam',[p,u],[handle,15],restype=d,check=False)
            finally:
                call('SetQHYCCDParam',[p,u,d],[handle,16,0])
        if args.frames:
            call('SetQHYCCDBinMode',[p,u,u],[handle,1,1])
            call('SetQHYCCDResolution',[p,u,u,u,u],[handle,0,0,w.value,h.value])
            call('SetQHYCCDParam',[p,u,d],[handle,8,args.seconds*1e6])
            call('SetQHYCCDSingleFrameTimeOut',[p,u],[handle,int(args.seconds*1000)+60000])
            length=call('GetQHYCCDMemLength',[p],[handle],check=False)
            if not w.value*h.value*2 <= length <= 512*1024*1024:
                raise RuntimeError(f'Unexpected image allocation {length}')
            pixels=(c.c_ubyte*length)()
            if args.cancel_first:
                call('ExpQHYCCDSingleFrame',[p],[handle],check=False)
                time.sleep(1)
                call('CancelQHYCCDExposingAndReadout',[p],[handle])
            for frame in range(args.frames):
                rc=call('ExpQHYCCDSingleFrame',[p],[handle],check=False)
                if rc not in (0,0x2000):
                    raise RuntimeError(f'Exposure failed {rc:#x}')
                ow,oh,ob,oc=u(),u(),u(),u()
                call('GetQHYCCDSingleFrame',[p,p,p,p,p,p],[handle,c.byref(ow),c.byref(oh),c.byref(ob),c.byref(oc),pixels])
                assert (ow.value,oh.value,ob.value,oc.value)==(w.value,h.value,16,1)
                data=c.string_at(pixels,w.value*h.value*2)
                output=args.output/f'frame-{frame}.raw'
                with output.open('xb') as f: f.write(data)
                print(json.dumps({'frame':frame,'bytes':len(data),'sha256':hashlib.sha256(data).hexdigest()}),flush=True)
    finally:
        if handle:
            call('CloseQHYCCD',[p],[handle],check=False)
        call('ReleaseQHYCCDResource',check=False)
        directory.close()


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sdk',type=Path,default=Path(r'C:\Program Files\QHYCCD\AllInOne\sdk\x64\qhyccd.dll'))
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--frames',type=int,default=3)
    parser.add_argument('--seconds',type=float,default=5)
    parser.add_argument('--mode',type=int,default=0)
    parser.add_argument('--wire',action='store_true',help='save bulk bytes separately for SDK image comparison')
    parser.add_argument('--map-controls',action='store_true',help='trace gain/offset/USB traffic then restore before full captures')
    parser.add_argument('--thermal',action='store_true',help='bounded manual cooling to 15 percent then off')
    parser.add_argument('--auto-cooler',action='store_true',help='trace SDK automatic target -5C for 30 seconds then off')
    parser.add_argument('--cancel-first',action='store_true',help='cancel a full-frame exposure after one second before the repeated captures')
    parser.add_argument('--child',action='store_true')
    args=parser.parse_args()
    if not 0<=args.frames<=20 or not 0<args.seconds<=30 or not 0<=args.mode<=10:
        parser.error('Invalid bounded experiment')
    if args.child:
        probe(args)
        return
    import frida
    args.output.mkdir(parents=True,exist_ok=False)
    with (args.output/'sdk.jsonl').open('x',encoding='utf-8') as log, (args.output/'usb.jsonl').open('x',encoding='utf-8') as trace:
        # Windows venv python.exe is a redirector which launches another process.
        # Attach the real interpreter, not that launcher, so SDK/USB hooks run.
        proc=subprocess.Popen([getattr(sys,'_base_executable',sys.executable),__file__,*sys.argv[1:],'--child'],stdin=subprocess.PIPE,stdout=log,stderr=subprocess.STDOUT)
        timer=threading.Timer(60+args.frames*(args.seconds+65),proc.kill)
        timer.start()
        session=None
        try:
            session=frida.attach(proc.pid)
            source='globalThis.QHY_CAPTURE_WIRE = '+json.dumps(args.wire)+';\n'+Path(__file__).with_name('trace-qhy.js').read_text(encoding='utf-8')
            script=session.create_script(source)
            wire=(args.output/'wire.bin').open('xb') if args.wire else None
            def record(message,data):
                if data and wire:
                    position=wire.tell();wire.write(data)
                    message['payload']['fileOffset']=position
                trace.write(json.dumps(message)+'\n'); trace.flush()
            script.on('message',record)
            script.load()
            proc.stdin.write(b'go\n');proc.stdin.flush()
            code=proc.wait()
            if wire: wire.close()
            print(json.dumps({'exitCode':code,'output':str(args.output),'sdkSha256':hashlib.sha256(args.sdk.read_bytes()).hexdigest()}))
            if code: raise SystemExit(code)
        finally:
            timer.cancel()
            if proc.poll() is None: proc.kill();proc.wait()
            if session:
                try: session.detach()
                except frida.InvalidOperationError: pass


if __name__=='__main__':
    main()
