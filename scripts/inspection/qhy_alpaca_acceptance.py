"""Full QHY600 RAW16 images through the production Alpaca HTTP endpoint."""
import argparse
import hashlib
import json
from pathlib import Path
import socket
import struct
import subprocess
import time
import urllib.request
import urllib.parse
import urllib.error

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--backend',choices=['sdk','direct'],required=True)
    parser.add_argument('--wheel',action='store_true')
    parser.add_argument('--usb-wheel',action='store_true')
    args=parser.parse_args();args.wheel=args.wheel or args.usb_wheel;args.output.mkdir(parents=True,exist_ok=False)
    root=Path(__file__).resolve().parents[2]
    with socket.socket() as listener:
        listener.bind(('127.0.0.1',0));port=listener.getsockname()[1]
    base=f'http://127.0.0.1:{port}'
    def request(path,data=None,binary=False):
        headers={}
        if data is not None:
            setup=path.startswith('/setup/')
            headers['Content-Type']='application/json' if setup else 'application/x-www-form-urlencoded'
            data=(json.dumps(data) if setup else urllib.parse.urlencode(data)).encode()
        if binary:headers['Accept']='application/imagebytes'
        method='GET' if data is None else ('POST' if path.startswith('/setup/') else 'PUT')
        with urllib.request.urlopen(urllib.request.Request(base+path,data,headers,method=method),timeout=40) as response:
            value=response.read()
        return value if binary else json.loads(value)
    def camera(member,data=None,binary=False):
        if data is not None:data=dict(data,ClientID=1)
        result=request('/api/v1/camera/0/'+member+'?ClientID=1',data,binary)
        if binary:return result
        assert result['ErrorNumber']==0,result
        return result.get('Value')
    def wheel(member,data=None):
        result=request(f'/api/v1/filterwheel/{1 if args.usb_wheel else 2}/'+member+'?ClientID=2',None if data is None else dict(data,ClientID=2))
        assert result['ErrorNumber']==0,result
        return result.get('Value')
    def move(position):
        start=time.monotonic();wheel('position',dict(Position=position))
        while wheel('position')==-1:
            assert time.monotonic()-start<45
            time.sleep(.15)
        assert wheel('position')==position
        print(json.dumps(dict(wheelPosition=position,seconds=time.monotonic()-start)),flush=True)
    results=[]
    with (args.output/'server.log').open('w') as log:
        process=subprocess.Popen([str(root/'target/release/regain-alpaca.exe'),'--no-discovery','--listen','127.0.0.1','--port',str(port),'--profiles',str(args.output.resolve()/'cameras.json')],stdout=log,stderr=log,creationflags=subprocess.CREATE_NO_WINDOW)
        try:
            until=time.monotonic()+20
            while True:
                try:state=request('/setup/api/state');break
                except urllib.error.URLError:
                    assert process.poll() is None and time.monotonic()<until
                    time.sleep(.1)
            profile=state['cameras'][0]['profile']
            profile.update(camera=dict(name='QHY600M',width=9600,height=6422,bins=[1]),serial='qhy600m-96763b4ab6a79e117',direct=args.backend=='direct',sdkFallback=False)
            profile['recovery']['maxRetries']=0
            if args.wheel and not args.usb_wheel:profile['filterWheel']=dict(enabled=True,names=['L','R','G','B','Ha','OIII','SII','Dark','Clear'],focusOffsets=list(range(9)))
            request('/setup/api/cameras/0',profile)
            if args.usb_wheel:
                wp=request('/setup/api/accessory/cfw')['profile']
                wp.update(serial='COM3',names=['L','R','G','B','Ha','OIII','SII','Dark','Clear'],focusOffsets=list(range(9)))
                request('/setup/api/accessory/cfw',wp)
            if args.wheel:
                devices=request('/management/v1/configureddevices')['Value']
                assert any(d['DeviceType']=='FilterWheel' and d['DeviceNumber']==(1 if args.usb_wheel else 2) for d in devices),devices
                wheel('connected',dict(Connected='true'))
                assert wheel('names')==(['L','R','G','B','Ha','OIII','SII','Dark','Clear'] if args.usb_wheel else profile['filterWheel']['names']) and wheel('focusoffsets')==list(range(9))
                for position in range(9):move(position)
            camera('connected',{'Connected':'true'})
            assert camera('name')=='QHY600M' and camera('exposuremax')==3600
            for index in range(3):
                if args.wheel:move(index*4)
                start=time.monotonic();camera('startexposure',{'Duration':5,'Light':'false'})
                if args.wheel and not args.usb_wheel:
                    rejected=request('/api/v1/filterwheel/2/position',dict(Position=1,ClientID=2))
                    assert rejected['ErrorNumber']==0x40B,rejected
                while not camera('imageready'):
                    assert time.monotonic()-start<40
                    time.sleep(.1)
                payload=camera('imagearray',binary=True)
                header=struct.unpack('<11I',payload[:44])
                assert header[:2]==(1,0) and header[4:]==(44,2,8,2,9600,6422,0),header
                assert len(payload)==44+9600*6422*2
                result=dict(frame=index,seconds=time.monotonic()-start,sha256=hashlib.sha256(payload[44:]).hexdigest())
                results.append(result);print(json.dumps(result),flush=True)
            assert len({r['sha256'] for r in results})==3
            if args.wheel:
                camera('connected',dict(Connected='false'))
                assert wheel('connected')
                move(0)
                camera('connected',dict(Connected='true'))
                wheel('connected',dict(Connected='false'))
                assert camera('connected')
                # Camera lease remains usable and the optical expectation remains armed.
                camera('startexposure',dict(Duration=5,Light='false'));start=time.monotonic()
                while not camera('imageready'):
                    assert time.monotonic()-start<40
                    time.sleep(.1)
                assert len(camera('imagearray',binary=True))==44+9600*6422*2
            diagnostics=json.loads(camera('action',{'Action':'Regain.Diagnostics','Parameters':''}))
            assert diagnostics['retry']['recaptures']==0,diagnostics
            (args.output/'results.json').write_text(json.dumps(results,indent=2))
        finally:
            try:
                camera('connected',{'Connected':'false'})
                if args.wheel:wheel('connected',dict(Connected='false'))
            finally:
                process.terminate();process.wait(timeout=15)

if __name__=='__main__':main()
