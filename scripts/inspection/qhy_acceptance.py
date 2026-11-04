"""Full-frame physical QHY600 acceptance through Regain's production supervisor."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import threading
import time

def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output',type=Path,required=True)
    p.add_argument('--backend',choices=['direct','sdk'],default='direct')
    p.add_argument('--cooling-recovery',action='store_true',help='Cool, close and reopen with remembered demand; verify another full frame')
    args=p.parse_args();args.output.mkdir(parents=True,exist_ok=False)
    root=Path(__file__).resolve().parents[2]
    log=(args.output/'worker.log').open('w',encoding='utf-8')
    proc=subprocess.Popen([str(root/'target/release/regain-alpaca.exe'),'--stdio','--backend',args.backend,'--sdk',str(root/'vendor/zwo/ASICamera2.dll')],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=log,creationflags=subprocess.CREATE_NO_WINDOW)
    timer=threading.Timer(300,proc.kill);timer.start();sequence=0;results=[]
    def exact(n):
        out=bytearray()
        while len(out)<n:
            part=proc.stdout.read(n-len(out))
            if not part:raise RuntimeError('Camera supervisor exited')
            out.extend(part)
        return bytes(out)
    def call(method,params=None):
        nonlocal sequence
        sequence+=1
        req=json.dumps(dict(version=1,id=sequence,method=method,params=params)).encode()
        proc.stdin.write(struct.pack('<I',len(req))+req);proc.stdin.flush()
        size=struct.unpack('<I',exact(4))[0];assert 0<size<=65536
        reply=json.loads(exact(size));assert reply['id']==sequence
        if not reply['ok']:raise RuntimeError(f'{method}: {reply}')
        n=reply['binaryLength'];assert 0<=n<=256*1024*1024
        return reply['result'],exact(n)
    exposure=dict(width=9600,height=6422,bin=1,x=0,y=0,microseconds=5_000_000,dark=True)
    try:
        opened,_=call('open',dict(name='QHY600M',serial='qhy600m-96763b4ab6a79e117',allowSdkFallback=False))
        (args.output/'open.json').write_text(json.dumps(opened,indent=2),encoding='utf-8')
        assert opened['info']['name']=='QHY600M'
        for index in range(3):
            start=time.monotonic();call('start',exposure)
            while True:
                status,_=call('status')
                if status['state']==3:raise RuntimeError(status)
                if status['state']==2:break
                if time.monotonic()-start>90:raise TimeoutError(status)
                time.sleep(.1)
            metadata,pixels=call('download');assert len(pixels)==9600*6422*2
            assert metadata['backend']==args.backend and metadata['recoveries']==0, metadata
            (args.output/f'frame-{index}.raw').write_bytes(pixels)
            result=dict(index=index,seconds=time.monotonic()-start,sha256=hashlib.sha256(pixels).hexdigest(),metadata=metadata)
            results.append(result);print(json.dumps(result),flush=True)
        assert len({r['sha256'] for r in results})==3
        # Cancellation must not poison a subsequent full frame or replay the old one.
        call('start',exposure);time.sleep(1);call('abort')
        start=time.monotonic();call('start',exposure)
        while True:
            status,_=call('status')
            if status['state']==3:raise RuntimeError(status)
            if status['state']==2:break
            if time.monotonic()-start>90:raise TimeoutError(status)
            time.sleep(.1)
        metadata,pixels=call('download');assert len(pixels)==9600*6422*2
        assert metadata['backend']==args.backend and metadata['recoveries']==0, metadata
        digest=hashlib.sha256(pixels).hexdigest();assert digest not in {r['sha256'] for r in results}
        results.append(dict(afterCancel=True,seconds=time.monotonic()-start,sha256=digest,metadata=metadata))
        (args.output/'after-cancel.raw').write_bytes(pixels)
        if args.cooling_recovery:
            call('set',dict(control=16,value=-5));call('set',dict(control=17,value=1))
            time.sleep(12)
            temperature,_=call('get',dict(control=8));power,_=call('get',dict(control=15))
            assert power>=25, (temperature,power)
            call('close')
            call('open',dict(name='QHY600M',serial='qhy600m-96763b4ab6a79e117',allowSdkFallback=False,
                 recoveryState=dict(settle=True,temperature=temperature/10,power=power,target=-5)))
            call('set',dict(control=16,value=-5));call('set',dict(control=17,value=1))
            start=time.monotonic();call('start',exposure)
            while True:
                status,_=call('status')
                if status['state']==3:raise RuntimeError(status)
                if status['state']==2:break
                if time.monotonic()-start>90:raise TimeoutError(status)
                time.sleep(.1)
            metadata,pixels=call('download');assert len(pixels)==9600*6422*2
            assert metadata['backend']==args.backend and metadata['recoveries']==0, metadata
            digest=hashlib.sha256(pixels).hexdigest();assert digest not in {r['sha256'] for r in results}
            results.append(dict(afterCoolerReopen=True,priorPower=power,priorTemperature=temperature/10,
                                seconds=time.monotonic()-start,sha256=digest,metadata=metadata))
            log.flush()
            assert 'cooling.resumed' in (args.output/'worker.log').read_text(encoding='utf-8'), 'Missing thermal recovery seed'
        call('close')
        (args.output/'results.json').write_text(json.dumps(results,indent=2),encoding='utf-8')
    finally:
        try:call('close')
        except Exception:pass
        proc.stdin.close()
        try:proc.wait(timeout=10)
        except subprocess.TimeoutExpired:proc.kill();proc.wait()
        timer.cancel();log.close()

if __name__=='__main__':main()
