"""Extract SDK A4 fields for every integer gain, excluding unused stack bytes."""
import argparse
import json
from pathlib import Path
import struct


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('artifacts',type=Path)
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args()
    maps={};sdk_hashes=set();cameras=set();firmware=set()
    for mode in [0,1,2,3,8,9]:
        stem=f'mode-{mode}-all-gains-01'
        summaries=[json.loads(line) for line in (args.artifacts/f'{stem}-console.log').read_text(encoding='utf-8-sig').splitlines()]
        assert summaries[-1]['exitCode']==0
        sdk_hashes.add(summaries[-1]['sdkSha256'])
        sdk_rows=[json.loads(line) for line in (args.artifacts/stem/'sdk.jsonl').read_text(encoding='utf-8').splitlines()]
        found=[r['cameras'] for r in sdk_rows if 'cameras' in r]
        assert len(found)==1 and len(found[0])==1
        cameras.add(tuple(found[0][0]))
        rows=[json.loads(line).get('payload',{}) for line in
              (args.artifacts/stem/'usb.jsonl').read_text(encoding='utf-8').splitlines()]
        gains={};pending={}
        for row in rows:
            thread=row.get('thread')
            if row.get('kind')=='control-complete' and row.get('header','').startswith('c0c2'):
                firmware.add(row['payload'][:6])
            if row.get('kind')=='sdk-enter' and row.get('name')=='SetQHYCCDParam':
                values=row['args']
                if int(values[1],16)==6:
                    gain=struct.unpack('<d',int(values[2],16).to_bytes(8,'little'))[0]
                    assert gain.is_integer() and 0<=gain<=200
                    pending[thread]=int(gain)
            if row.get('kind')=='control-output' and row['payload'].startswith('a4') and thread in pending:
                gain=pending.pop(thread)
                payload=bytes.fromhex(row['payload'])
                assert len(payload)==32
                fields=payload[1:17]
                # Only these six bytes differ across the traced gain/mode matrix.
                assert all(value==0 for i,value in enumerate(fields) if i not in [0,1,2,3,13,15])
                record=[int.from_bytes(fields[:2],'big'),int.from_bytes(fields[2:4],'big'),fields[13],fields[15]]
                assert gain not in gains or gains[gain]==record
                gains[gain]=record
        assert set(gains)==set(range(201)), (mode,len(gains))
        maps[str(mode)]=[gains[gain] for gain in range(201)]
    assert len(sdk_hashes)==len(cameras)==len(firmware)==1, 'Mixed hardware or SDK references'
    result=dict(sdkSha256=next(iter(sdk_hashes)),camera=list(next(iter(cameras))),firmwareDateBytes=next(iter(firmware)),
                columns=['analog','digital','payload14','payload16'],gains=maps)
    args.output.parent.mkdir(parents=True,exist_ok=True)
    # Keep one gain row per line for review without thousands of scalar lines.
    text=json.dumps(result,indent=2)
    import re
    text=re.sub(r'\[\s+(\d+),\s+(\d+),\s+(\d+),\s+(\d+)\s+\]',r'[\1, \2, \3, \4]',text)
    args.output.write_text(text+'\n',encoding='utf-8')
    print(json.dumps(dict(modes=len(maps),gainsPerMode=201,output=str(args.output))))


if __name__=='__main__':
    main()
