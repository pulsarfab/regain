"""Opt-in QHY600M + 9-slot CFW3 camera-bridge acceptance, full RAW16 frames."""
import argparse, hashlib, json, struct, subprocess, threading, time
from pathlib import Path

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--backend',choices=['direct','sdk'],required=True)
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args();args.output.mkdir(parents=True,exist_ok=False)
    root=Path(__file__).resolve().parents[2]
    log=(args.output/'worker.log').open('w',encoding='utf-8')
    proc=subprocess.Popen([str(root/'target/release/regain-alpaca.exe'),'--stdio','--backend',args.backend],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=log,creationflags=subprocess.CREATE_NO_WINDOW)
    timer=threading.Timer(300,proc.kill);timer.start();sequence=0;results=[]
    def exact(n):
        data=bytearray()
        while len(data)<n:
            chunk=proc.stdout.read(n-len(data))
            if not chunk: raise RuntimeError('Supervisor exited')
            data.extend(chunk)
        return bytes(data)
    def call(method,params=None,reject=False):
        nonlocal sequence
        sequence+=1
        data=json.dumps(dict(version=1,id=sequence,method=method,params=params)).encode()
        proc.stdin.write(struct.pack('<I',len(data))+data);proc.stdin.flush()
        reply=json.loads(exact(struct.unpack('<I',exact(4))[0]));assert reply['id']==sequence
        pixels=exact(reply.get('binaryLength',0))
        if reject:
            assert not reply['ok'],reply
            results.append(dict(rejected=method,reply=reply));return reply,None
        assert reply['ok'],reply
        return reply['result'],pixels
    identity=dict(name='QHY600M',serial='qhy600m-96763b4ab6a79e117',allowSdkFallback=False)
    exposure=dict(width=9600,height=6422,bin=1,x=0,y=0,microseconds=5_000_000,dark=True)
    def move(position):
        start=time.monotonic();call('wheel-move',dict(position=position))
        while True:
            state,_=call('wheel-status')
            if not state['moving']:
                assert state['verified'] and state['position']==position,state
                break
            assert time.monotonic()-start<45,state
            time.sleep(.1)
        result=dict(move=position,seconds=time.monotonic()-start,state=state);results.append(result);print(json.dumps(result),flush=True)
    def capture(position):
        start=time.monotonic();call('start',exposure)
        time.sleep(.2);call('wheel-move',dict(position=(position+1)%9),reject=True)
        while True:
            state,_=call('status');assert state['state']!=3,state
            if state['state']==2:break
            assert time.monotonic()-start<60,state
            time.sleep(.1)
        metadata,pixels=call('download');assert len(pixels)==9600*6422*2
        assert metadata['backend']==args.backend and metadata['recoveries']==0,metadata
        wheel,_=call('wheel-status');assert wheel['verified'] and wheel['position']==position,wheel
        result=dict(captureAt=position,seconds=time.monotonic()-start,sha256=hashlib.sha256(pixels).hexdigest(),metadata=metadata)
        results.append(result);print(json.dumps(result),flush=True)
    try:
        call('open',identity);initial,_=call('wheel-status');assert initial['slots']==9 and initial['verified'],initial
        before,_=call('diagnostics')
        for target in list(range(9))+list(range(8,-1,-1))+[8,0,4,4,0]:move(target)
        for target in [0,4,8]:move(target);capture(target)
        # Wrong persisted expectation must block, without an automatic move or recapture.
        call('close');call('open',dict(**identity,wheelExpected=dict(slots=9,position=3)))
        state,_=call('wheel-status');assert state['position']==8 and not state['verified'],state
        call('start',exposure)
        start=time.monotonic()
        while True:
            state,_=call('status')
            if state['state']==3:break
            assert state['state']!=2 and time.monotonic()-start<15,state
            time.sleep(.1)
        results.append(dict(opticalMismatchBlocked=state))
        state,_=call('wheel-status');assert state['position']==8,state
        call('abort');move(0);capture(0)
        after,_=call('diagnostics');results.append(dict(before=before,after=after))
        call('close')
        assert len({r['sha256'] for r in results if 'sha256' in r})==4
        (args.output/'results.json').write_text(json.dumps(results,indent=2),encoding='utf-8')
    finally:
        try:call('close')
        except Exception:pass
        proc.stdin.close()
        try:proc.wait(timeout=10)
        except subprocess.TimeoutExpired:proc.kill();proc.wait()
        timer.cancel();log.close()

if __name__=='__main__':main()
