"""Alpaca EFW/EAF contracts against the production native worker's simulator."""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.request
import urllib.parse
import urllib.error

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir',default='target/debug')
parser.add_argument('--hardware',action='store_true',help='exercise attached EFW and unmounted EAF with short moves')
parser.add_argument('--calibrate',action='store_true',help='also calibrate the EFW on hardware; always tested in simulation')
args=parser.parse_args()
with socket.socket() as listener:
    listener.bind(('127.0.0.1',0));port=listener.getsockname()[1]
base=f'http://127.0.0.1:{port}'
def request(path,data=None,method=None):
    headers={}
    if data is not None:
        setup=path.startswith('/setup/')
        headers['Content-Type']='application/json' if setup else 'application/x-www-form-urlencoded'
        data=(json.dumps(data) if setup else urllib.parse.urlencode(data)).encode()
    with urllib.request.urlopen(urllib.request.Request(base+path,data,headers,method=method),timeout=20) as response:
        return json.load(response)
def api(device,member,values=None,client=1,error=0):
    p={'ClientID':client,'ClientTransactionID':42};p.update(values or {})
    path='/api/v1/'+device+'/0/'+member
    value=request(path+('?' + urllib.parse.urlencode(p) if values is None else ''),None if values is None else p,'GET' if values is None else 'PUT')
    assert value['ErrorNumber']==error and value['ClientTransactionID']==42,value
    return value.get('Value')
with tempfile.TemporaryDirectory(prefix='zwogain-accessories-') as directory:
    profiles=Path(directory)/'cameras.json'
    binary=Path(args.bin_dir).resolve()/('zwogain-alpaca.exe' if os.name=='nt' else 'zwogain-alpaca')
    with (Path(directory)/'log.txt').open('w+b') as log:
        process=subprocess.Popen([str(binary),*([] if args.hardware else ['--simulate']),'--no-discovery','--port',str(port),'--profiles',str(profiles)],stdout=log,stderr=log)
        try:
            deadline=time.monotonic()+30
            while True:
                try: request('/setup/api/accessory/efw');break
                except urllib.error.URLError:
                    assert process.poll() is None and time.monotonic()<deadline
                    time.sleep(.05)
            for kind,device,version in [('efw','filterwheel',2),('eaf','focuser',3)]:
                setup='/setup/api/accessory/'+kind
                assert api(device,'interfaceversion')==version
                assert ('ZwoGain.Calibrate' in api(device,'supportedactions')) == (kind=='efw')
                api(device,'position',error=0x407)
                choices=request(setup+'/discover',{})
                profile=request(setup)['profile'];profile['serial']=choices[0]['identity']['serial']
                request(setup,profile)
                api(device,'connected',{'Connected':True})
                api(device,'connected',{'Connected':True},client=2)
                api(device,'connected',{'Connected':False})
                assert api(device,'connected',client=2)
                api(device,'position',error=0x407)
                api(device,'connected',{'Connected':True})
                original=json.loads(api(device,'action',{'Action':'ZwoGain.Status','Parameters':''}))
                if kind=='efw':
                    assert len(api(device,'names'))==7 and api(device,'focusoffsets')==[0]*7
                    if args.calibrate or not args.hardware:
                        names=api(device,'names'); offsets=api(device,'focusoffsets')
                        started=time.monotonic()
                        assert api(device,'action',{'Action':'ZwoGain.Calibrate','Parameters':''})=='null'
                        active=json.loads(api(device,'action',{'Action':'ZwoGain.Status','Parameters':''}))
                        assert active['calibrating'] and active['slots']==7 and api(device,'position')==-1,active
                        api(device,'action',{'Action':'ZwoGain.Calibrate','Parameters':''},error=0x500)
                        api(device,'position',{'Position':1},error=0x500)
                        while True:
                            status=json.loads(api(device,'action',{'Action':'ZwoGain.Status','Parameters':''},client=2))
                            assert status['slots']==7 and not status['fault'] and not status['error'],status
                            if not status['calibrating']: break
                            assert time.monotonic()-started<95
                            time.sleep(.1)
                        assert status['position']==0 and not status['moving'],status
                        assert api(device,'names')==names and api(device,'focusoffsets')==offsets
                        print('Alpaca EFW calibration passed in',round(time.monotonic()-started,3),'seconds')
                    for pos in range(7):
                        api(device,'position',{'Position':pos})
                        deadline=time.monotonic()+30
                        while api(device,'position')==-1:
                            assert time.monotonic()<deadline
                            time.sleep(.1)
                        assert api(device,'position')==pos
                    api(device,'position',{'Position':7},error=0x401)
                    api(device,'position',{'Position':original['position']})
                    deadline=time.monotonic()+30
                    while api(device,'position')==-1:
                        assert time.monotonic()<deadline
                        time.sleep(.1)
                else:
                    api(device,'action',{'Action':'ZwoGain.Calibrate','Parameters':''},error=0x40c)
                    assert api(device,'absolute') and not api(device,'tempcompavailable')
                    assert api(device,'maxstep')==60000
                    assert -50<api(device,'temperature')<100
                    target=original['position']+50
                    api(device,'move',{'Position':target})
                    deadline=time.monotonic()+15
                    while api(device,'ismoving'):
                        assert time.monotonic()<deadline
                        time.sleep(.1)
                    assert api(device,'position')==target
                    api(device,'move',{'Position':original['position']})
                    while api(device,'ismoving'):
                        assert time.monotonic()<deadline
                        time.sleep(.1)
                    api(device,'halt',{})
                    api(device,'move',{'Position':-1},error=0x401)
                    api(device,'stepsize',error=0x400)
                    request(setup+'/settings',{'beep':False,'backlash':5})
                    status=json.loads(api(device,'action',{'Action':'ZwoGain.Status','Parameters':''}))
                    assert not status['beep'] and status['backlash']==5
                    request(setup+'/settings',{'beep':original['beep'],'backlash':original['backlash']})
                api(device,'action',{'Action':'bad','Parameters':''},error=0x40c)
                api(device,'connected',{'Connected':False},client=2)
                api(device,'connected',{'Connected':False})
                saved=json.loads(profiles.with_suffix('.'+kind+'.json').read_text())
                assert saved['serial']==profile['serial']
                if kind=='efw':
                    saved['names'][0]='Luminance';saved['focusOffsets'][1]=25
                    request(setup,saved);api(device,'connected',{'Connected':True})
                    assert api(device,'names')[0]=='Luminance' and api(device,'focusoffsets')[1]==25
                    api(device,'connected',{'Connected':False})
            devices=request('/management/v1/configureddevices')['Value']
            assert {d['DeviceType'] for d in devices}=={'FilterWheel','Focuser'}
            print('Alpaca EFW/EAF contracts passed: discovery, profiles, clients, motion, settings, errors and metadata.')
        finally:
            process.terminate();process.wait(timeout=10)
