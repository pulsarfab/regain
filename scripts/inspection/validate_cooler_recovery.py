"""Measure an owned ASI585MM Pro cooling recovery, with explicit hardware opt-in."""
import argparse, hashlib, importlib.util, json, subprocess, threading, time
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('pipe',ROOT/'scripts/test-rust.py'); pipe=importlib.util.module_from_spec(spec);spec.loader.exec_module(pipe)
p=argparse.ArgumentParser();p.add_argument('--hardware',action='store_true',required=True);p.add_argument('--workers',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--target',type=int,default=15);p.add_argument('--serial',required=True);args=p.parse_args()
args.output.mkdir(parents=True,exist_ok=False)
with pipe.Worker([str(args.workers.resolve()/'regain-alpaca.exe'),'--stdio','--backend','direct']) as w, (args.output/'samples.jsonl').open('x') as log:
 w.timer.cancel();w.timer=threading.Timer(480,w.process.kill);w.timer.start()
 def call(method,params=None): return w.call(method,params)[0]
 def record(**row): log.write(json.dumps(row)+'\n');log.flush(); print(json.dumps(row),flush=True)
 record(kind='build',sha256={n:hashlib.sha256((args.workers/n).read_bytes()).hexdigest() for n in ('regain-alpaca.exe','regain-device.exe')})
 recovery=dict(maxRetries=1,reconnectDelaySeconds=2,coolingStableSamples=3,coolingSampleSeconds=1,coolingTimeoutSeconds=180)
 opened=call('open',dict(name='ZWO ASI585MM Pro',serial=args.serial,recovery=recovery))
 controls={c['type']:c['value'] for c in opened['controls']}; record(kind='initial',controls=controls)
 try:
  call('set',dict(control=16,value=args.target)); call('set',dict(control=17,value=1))
  start=time.monotonic()
  for second in range(120):
   s=call('status');record(kind='cooldown',second=second,**{k:s[k] for k in ('phase','environment','controlConnectionAvailable')})
   time.sleep(1)
  prior=call('status');child=call('diagnostics')['processId']; record(kind='before-fault',child=child,status=prior)
  call('start',dict(width=64,height=64,bin=1,x=0,y=0,microseconds=2000000,dark=True));time.sleep(.5)
  subprocess.run(['taskkill','/PID',str(child),'/F'],check=True,capture_output=True)
  start=time.monotonic();ready=None
  for second in range(180):
   s=call('status');record(kind='recovery',second=round(time.monotonic()-start,2),**{k:s[k] for k in ('phase','environment','controlConnectionAvailable','state','error')})
   if s['state']==2 and ready is None: ready=time.monotonic()-start;record(kind='image-ready',seconds=ready); call('download')
   if s['state']==3: raise RuntimeError(s)
   if ready is not None and time.monotonic()-start>ready+30:break
   time.sleep(1)
  assert ready is not None, 'capture did not recover'
 finally:
  try:call('abort');call('set',dict(control=17,value=0));call('set',dict(control=16,value=controls[16]));call('close')
  finally:w.log.seek(0);(args.output/'worker.log').write_bytes(w.log.read())
