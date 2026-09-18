"""Reproduce the CAA NTC table from the inspected, MIT-licensed SDK DLL."""
import argparse
import hashlib
from pathlib import Path
import struct
import pefile

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('sdk',type=Path)
parser.add_argument('--output',type=Path,required=True)
args=parser.parse_args()
data=args.sdk.read_bytes()
if hashlib.sha256(data).hexdigest()!='413d629adfd81150962211d2241aec2a99420ba2d65c0161f7700da32d534152':
    parser.error('requires inspected CAA 1.5.9 x64 DLL')
pe=pefile.PE(data=data)
rows=[struct.unpack('<i4xd',pe.get_data(0xe190+16*i,16)) for i in range(271)]
assert [t for t,r in rows]==list(range(-20,251))
assert all(a[1]>b[1]>0 for a,b in zip(rows,rows[1:]))
with args.output.open('x',encoding='utf-8') as output:
    output.write('// NTC resistance table from CAA SDK 1.5.9; see ../LICENSE-ZWO.\n')
    output.write('// Temperatures -20 through 250 C; resistance in kilohms.\n')
    output.write('const RESISTANCE: [f64; 271] = [\n')
    for _,resistance in rows: output.write(f'    {resistance!r},\n')
    output.write('];\n')
