"""Trace QHY camera/4-pin CFW commands in an owned SDK process, never COM ports."""
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
    input()  # Parent attaches passive USB tracing before loading the SDK.
    directory = os.add_dll_directory(str(args.sdk.parent))
    sdk = c.WinDLL(str(args.sdk))
    u, p = c.c_uint32, c.c_void_p
    def call(name, types=(), values=(), result=u, check=True):
        fn = getattr(sdk, name); fn.argtypes = types; fn.restype = result
        start = time.monotonic(); value = fn(*values)
        print(json.dumps(dict(api=name, result=value, elapsedMs=(time.monotonic()-start)*1000)), flush=True)
        if check and value != 0: raise RuntimeError(f'{name}: {value}')
        return value
    call('EnableQHYCCDMessage', [c.c_bool], [False], result=None, check=False)
    call('EnableQHYCCDLogFile', [c.c_bool], [False], result=None, check=False)
    call('InitQHYCCDResource')
    handle = None
    try:
        count = call('ScanQHYCCD', check=False)
        assert 0 < count < 32
        found = []
        for index in range(count):
            identity, model = c.create_string_buffer(256), c.create_string_buffer(256)
            call('GetQHYCCDId', [u,p], [index,identity])
            call('GetQHYCCDModel', [p,p], [identity,model])
            if identity.value.decode().lower() == args.camera.lower(): found.append(identity.value)
        assert len(found) == 1, 'Select one exact camera ID'
        handle = call('OpenQHYCCD', [p], [found[0]], result=p, check=False)
        assert handle
        call('SetQHYCCDReadMode', [p,u], [handle,0])
        call('SetQHYCCDStreamMode', [p,c.c_uint8], [handle,0])
        call('InitQHYCCD', [p], [handle])
        call('IsQHYCCDCFWPlugged', [p], [handle])
        call('IsQHYCCDControlAvailable', [p,u], [handle,44])
        slots = call('GetQHYCCDParam', [p,u], [handle,44], result=c.c_double, check=False)
        assert slots.is_integer() and 2 <= slots <= 16, slots
        slots = int(slots)
        def position():
            status = c.create_string_buffer(64)
            call('GetQHYCCDCFWStatus', [p,p], [handle,status])
            raw = status.raw[:4]
            print(json.dumps(dict(cfwStatusHex=raw.hex(), cfwStatusText=raw.decode('ascii',errors='replace'))), flush=True)
            return status.raw[0]
        initial = position()
        print(json.dumps(dict(camera=args.camera,slots=slots,initialStatus=initial)),flush=True)
        if args.sweep or args.captures or args.reconnect:
            digits=b'0123456789ABCDEF'
            assert initial in digits[:slots], f'Unrecognized initial status {initial}'
            initial_index=digits.index(initial)
            targets=(list(range(slots))+list(reversed(range(slots)))+[slots-1,0,slots//2,0,initial_index,initial_index]
                     if args.sweep else [0,slots//2,slots-1,initial_index] if args.captures else [slots//2,initial_index])
            moves=[]
            frames=[]
            for target in targets:
                order=c.create_string_buffer(bytes([digits[target]]))
                print(json.dumps(dict(move=target)),flush=True)
                start=time.monotonic()
                call('SendOrder2QHYCCDCFW',[p,p,u],[handle,order,1])
                while True:
                    actual=position()
                    if actual==digits[target]: break
                    assert actual in digits[:slots] or actual==ord('N'), f'Invalid wheel status {actual}'
                    if time.monotonic()-start>45: raise TimeoutError(f'Wheel target {target}, status {actual}; no replay')
                    time.sleep(.25)
                time.sleep(.25)
                assert position()==digits[target], 'Completion did not remain stable'
                entry=dict(target=target,elapsedSeconds=time.monotonic()-start)
                moves.append(entry);print(json.dumps(entry),flush=True)
                if args.reconnect and len(moves)==1:
                    call('CloseQHYCCD',[p],[handle]);handle=None
                    handle=call('OpenQHYCCD',[p],[found[0]],result=p,check=False)
                    assert handle
                    call('SetQHYCCDReadMode',[p,u],[handle,0])
                    call('SetQHYCCDStreamMode',[p,c.c_uint8],[handle,0])
                    call('InitQHYCCD',[p],[handle])
                    call('IsQHYCCDCFWPlugged',[p],[handle])
                    assert call('GetQHYCCDParam',[p,u],[handle,44],result=c.c_double,check=False)==slots
                    assert position()==digits[target], 'Reopening camera changed optical position'
                    print(json.dumps(dict(reconnectedPosition=target,verified=True)),flush=True)
                if args.captures and len(frames)<3:
                    # Full QHY600 sensor only. Test normal sequencing: settled
                    # wheel -> five-second capture -> unchanged wheel readback.
                    call('SetQHYCCDBitsMode',[p,u],[handle,16])
                    call('SetQHYCCDBinMode',[p,u,u],[handle,1,1])
                    call('SetQHYCCDResolution',[p,u,u,u,u],[handle,0,0,9600,6422])
                    call('SetQHYCCDParam',[p,u,c.c_double],[handle,8,5_000_000.])
                    call('SetQHYCCDSingleFrameTimeOut',[p,u],[handle,65000])
                    capacity=call('GetQHYCCDMemLength',[p],[handle],check=False)
                    expected=9600*6422*2
                    assert expected<=capacity<=512*1024*1024
                    pixels=(c.c_ubyte*capacity)()
                    started=time.monotonic()
                    assert call('ExpQHYCCDSingleFrame',[p],[handle],check=False) in (0,0x2000)
                    width,height,bits,channels=u(),u(),u(),u()
                    call('GetQHYCCDSingleFrame',[p,p,p,p,p,p],[handle,c.byref(width),c.byref(height),c.byref(bits),c.byref(channels),pixels])
                    assert (width.value,height.value,bits.value,channels.value)==(9600,6422,16,1)
                    data=c.string_at(pixels,expected)
                    digest=hashlib.sha256(data).hexdigest()
                    assert digest not in {f['sha256'] for f in frames}, 'Stale camera image'
                    (args.output/f'frame-{len(frames)}.raw').write_bytes(data)
                    frames.append(dict(position=target,bytes=expected,sha256=digest,seconds=time.monotonic()-started))
                    assert position()==digits[target], 'Wheel position changed during capture'
                    del pixels,data
            (args.output/'moves.json').write_text(json.dumps(moves,indent=2))
            if args.captures: (args.output/'frames.json').write_text(json.dumps(frames,indent=2))
            assert position()==initial
        (args.output/'identity.json').write_text(json.dumps(dict(camera=args.camera,slots=slots,initialStatus=initial),indent=2))
    finally:
        if handle: call('CloseQHYCCD',[p],[handle])
        call('ReleaseQHYCCDResource')
        directory.close()


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sdk',type=Path,default=Path(r'C:\Program Files\QHYCCD\AllInOne\sdk\x64\qhyccd.dll'))
    parser.add_argument('--camera',required=True)
    parser.add_argument('--output',required=True,type=Path)
    parser.add_argument('--sweep',action='store_true')
    parser.add_argument('--captures',action='store_true',help='Three full five-second RAW16 frames at settled filter positions')
    parser.add_argument('--reconnect',action='store_true',help='Verify a non-home slot survives camera close/reopen, then restore')
    parser.add_argument('--child',action='store_true')
    args=parser.parse_args()
    if sum([args.sweep,args.captures,args.reconnect])>1: parser.error('Select one physical acceptance mode')
    if args.child: return probe(args)
    import frida
    args.output.mkdir(parents=True,exist_ok=False)
    with (args.output/'sdk.jsonl').open('x') as log,(args.output/'usb.jsonl').open('x') as trace:
        proc=subprocess.Popen([getattr(sys,'_base_executable',sys.executable),__file__,*sys.argv[1:],'--child'],stdin=subprocess.PIPE,stdout=log,stderr=subprocess.STDOUT,creationflags=subprocess.CREATE_NO_WINDOW)
        timer=threading.Timer(900 if args.sweep or args.captures else 90,proc.kill);timer.start()
        session=None
        try:
            session=frida.attach(proc.pid)
            script=session.create_script(Path(__file__).with_name('trace-qhy.js').read_text())
            def record(message,data):
                trace.write(json.dumps(message)+'\n');trace.flush()
            script.on('message',record);script.load()
            proc.stdin.write(b'go\n');proc.stdin.flush()
            code=proc.wait()
            print(json.dumps(dict(exitCode=code,output=str(args.output),sdkSha256=hashlib.sha256(args.sdk.read_bytes()).hexdigest())))
            if code: raise SystemExit(code)
        finally:
            timer.cancel()
            if proc.poll() is None: proc.kill();proc.wait()
            if session:
                try: session.detach()
                except frida.InvalidOperationError: pass


if __name__=='__main__': main()
