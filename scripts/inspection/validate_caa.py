"""Trace a native CAA worker and test bounded recovery from uncertain HID I/O.

Faults change a successful Windows HID call's returned result, after the real
device transaction. They do not disconnect USB. The move is at most 2 degrees.
Raw trace output includes the device serial and should remain local.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import threading
import time
import frida

HOOK = r'''
const hid = Module.load('hid.dll');
let failInput=false, failMove=false;
rpc.exports = {
  failInput() { failInput=true; },
  failMove() { failMove=true; }
};
for (const name of ['HidD_SetOutputReport','HidD_GetInputReport']) {
  Interceptor.attach(hid.getExportByName(name), {
    onEnter(args) {
      this.buffer=args[1]; this.length=args[2].toUInt32();
      if (this.length>128) throw new Error('oversized CAA report');
      this.before=Array.from(new Uint8Array(this.buffer.readByteArray(this.length)));
    },
    onLeave(result) {
      const bytes=name==='HidD_SetOutputReport'?this.before:Array.from(new Uint8Array(this.buffer.readByteArray(this.length)));
      let injected=false;
      if (name==='HidD_GetInputReport' && failInput) { failInput=false; injected=true; }
      if (name==='HidD_SetOutputReport' && bytes[3]===3 && bytes[4]===1 && failMove) { failMove=false; injected=true; }
      const actual=result.toInt32()!==0;
      if (injected) { result.replace(0); this.lastError=31; }
      send({kind:'hid',name,length:this.length,bytes,actualSuccess:actual,injectedFailure:injected});
    }
  });
}
send({kind:'modules',sdkLoaded:Process.enumerateModules().some(m=>/CAA_SRC|ASICamera2/i.test(m.name))});
send({kind:'hooks-ready'});
'''

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--worker',type=Path,default=Path('target/release/regain-caa.exe'))
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args()
    args.output.parent.mkdir(parents=True,exist_ok=True)
    with args.output.open('x',encoding='utf-8') as log:
        events=[]; lock=threading.Lock()
        def record(value):
            with lock:
                events.append(value)
                log.write(json.dumps({'time':time.time(),**value})+'\n'); log.flush()
        record({'kind':'configuration','workerSha256':hashlib.sha256(args.worker.read_bytes()).hexdigest()})
        proc=subprocess.Popen([str(args.worker.resolve()),'serve'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        timer=threading.Timer(60,proc.kill); timer.start()
        session=None; initial=None; script=None; identity=None
        def request(command,expect=True,**fields):
            value={'command':command,**fields}; record({'kind':'request',**value})
            proc.stdin.write(json.dumps(value)+'\n'); proc.stdin.flush()
            reply=json.loads(proc.stdout.readline())
            record({'kind':'reply','command':command,**reply})
            assert reply['ok']==expect,reply
            return reply.get('result')
        def wait(target):
            deadline=time.monotonic()+15
            while time.monotonic()<deadline:
                s=request('status')
                assert s['error']==0,s
                if not s['moving'] and abs(s['mechanical_degrees']-target)<=.1: return s
                time.sleep(.1)
            raise TimeoutError('CAA move did not finish')
        try:
            # Startup handshake also confirms we are attaching to the worker
            # after it has acquired the CAA's exclusive HID handle.
            request('settings')
            second=subprocess.run([str(args.worker.resolve()),'status'],capture_output=True,text=True,timeout=5)
            assert second.returncode!=0 and 'exclusively' in second.stderr
            record({'kind':'exclusive-open','secondOpenRejected':True})
            session=frida.attach(proc.pid)
            script=session.create_script(HOOK)
            ready=threading.Event()
            def message(value,data):
                payload=value.get('payload',{'kind':'trace-error','message':value})
                record(payload)
                if payload.get('kind')=='hooks-ready': ready.set()
            script.on('message',message); script.load()
            assert ready.wait(5),'HID hooks unavailable'
            initial=request('status'); settings=request('settings'); identity=request('identity')
            assert not initial['moving'] and initial['error']==0
            assert len(identity['alias'])<=8 and all(32<=ord(c)<=126 for c in identity['alias']), 'cannot round-trip this alias safely'
            request('alias',text='ZGTEST')
            assert request('identity')['alias']=='ZGTEST'
            request('alias',text=identity['alias'])
            assert request('identity')==identity
            script.exports_sync.fail_input()
            request('status',expect=False)
            assert request('status')['mechanical_degrees']==initial['mechanical_degrees']
            start=initial['mechanical_degrees']
            target=start+2 if start+2<=initial['limit_degrees'] else start-2
            assert target>=0
            script.exports_sync.fail_move()
            request('move-mechanical',expect=False,degrees=target)
            reached=wait(target)
            record({'kind':'uncertain-write-reached','status':reached})
            # Before sending any restoration, there must be exactly one motion
            # command: the failed return cannot cause automatic replay.
            time.sleep(.1)
            with lock:
                motion=[e for e in events if e.get('kind')=='hid' and e['name']=='HidD_SetOutputReport' and e['bytes'][3:5]==[3,1]]
            assert len(motion)==1 and motion[0]['actualSuccess'] and motion[0]['injectedFailure'],motion
            request('move-mechanical',degrees=start); final=wait(start)
            assert request('settings')==settings
            assert request('identity')==identity
            with lock:
                assert any(e.get('kind')=='modules' and e['sdkLoaded'] is False for e in events)
                assert not any(e.get('kind')=='trace-error' for e in events)
                assert sum(e.get('injectedFailure',False) for e in events)==2
            record({'kind':'complete','motionCommandsBeforeRestoration':len(motion),'final':final})
        finally:
            try:
                if initial is not None and proc.poll() is None:
                    cleanup_errors=[]
                    for command,fields in [('stop',{}),('alias',{'text':identity['alias']})] if identity else [('stop',{})]:
                        try: request(command,**fields)
                        except Exception as error: cleanup_errors.append(str(error))
                    try:
                        request('move-mechanical',degrees=initial['mechanical_degrees'])
                        wait(initial['mechanical_degrees'])
                    except Exception as error: cleanup_errors.append(str(error))
                    record({'kind':'cleanup','errors':cleanup_errors})
                    assert not cleanup_errors,cleanup_errors
            finally:
                try:
                    if proc.poll() is None:
                        proc.stdin.close()
                        try: proc.wait(timeout=5)
                        except subprocess.TimeoutExpired: proc.kill(); proc.wait()
                finally:
                    timer.cancel()
                    if session is not None:
                        try: session.detach()
                        except frida.InvalidOperationError: pass
            if proc.returncode: raise RuntimeError(proc.stderr.read())
    print(args.output)

if __name__=='__main__': main()
