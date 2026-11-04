"""Drive the opt-in installed NINA fixture after connecting the QHY camera/wheel.

Run once per backend, using the already selected temporary NINA profile.
The fixture drives real NINA mediators and saves full-frame FITS files.
"""
import argparse,json,time
from pathlib import Path

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--backend',choices=['direct','sdk'],required=True)
    parser.add_argument('--usb-wheel',action='store_true')
    parser.add_argument('--run-label',default='')
    args=parser.parse_args();root=args.output;prefix=('usb-' if args.usb_wheel else '')+args.backend+('-'+args.run_label if args.run_label else '')
    assert (root/'ready').exists(),'Installed NINA fixture is not ready'
    def call(tag,command,**parameters):
        tag=prefix+'-'+tag;path=root/(tag+'.json');assert not path.exists(),tag
        assert not (root/'request.json').exists(),'Another fixture request is active'
        temp=root/'request.tmp';temp.write_text(json.dumps(dict(id=tag,command=command,**parameters)),encoding='utf-8');temp.replace(root/'request.json')
        until=time.monotonic()+90
        while not path.exists():
            if time.monotonic()>until:raise TimeoutError(tag)
            time.sleep(.2)
        result=json.loads(path.read_text(encoding='utf-8-sig'));print(json.dumps(result),flush=True);assert result['ok'],result
    for slot in range(9):
        call('move-'+str(slot),'move-wheel',position=slot)
        if slot in [0,4,8]:call('frame-'+str(slot),'capture')
    if not args.usb_wheel:call('guard','capture-reject-wheel')
    call('camera-off','disconnect-camera')
    call('wheel-only','move-wheel',position=2)
    call('camera-on','connect-camera')
    call('frame-reconnect-camera','capture')
    call('wheel-off','disconnect-wheel')
    if not args.usb_wheel:call('frame-wheel-off','capture')
    call('wheel-on','connect-wheel')
    call('restored','move-wheel',position=0)
    call('state','wheel-state')
    if args.usb_wheel:call('frame-final','capture')

if __name__=='__main__':main()
