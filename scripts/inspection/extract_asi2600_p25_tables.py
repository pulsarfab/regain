"""Extract reviewed ASI2600MM Pro P25 volatile setup from the SDK 1.41 trace."""
import argparse
import hashlib
import json
from pathlib import Path
import struct

TRACE_SHA256 = '127e92828c2070b253abf57f365d84e5f5f0519474bea6f6d11cde43b14c7f58'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('trace', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    raw = args.trace.read_bytes()
    if hashlib.sha256(raw).hexdigest() != TRACE_SHA256:
        parser.error('requires the reviewed asi2600-p25-mapped-01 trace')
    lines = ['//! ASI2600MM Pro P25 volatile initialization observed with SDK 1.41.',
             '//! Reproduce with scripts/inspection/extract_asi2600_p25_tables.py.',
             '//! No calibration, serial, EEPROM, or firmware payloads.',
             'pub const INITIALIZE: &[(u8, u16, u16)] = &[']
    for event in map(json.loads, raw.splitlines()):
        if event['kind'] != 'io-submit' or event['code'] != '0x220020' or not 303 <= event['sequence'] <= 529:
            continue
        direction, request, register, value, length = struct.unpack('<BBHHH', bytes.fromhex(event['header'])[:8])
        if direction != 0x40:
            continue
        if request not in (0xb6, 0xbd, 0xaf) or length != 0:
            raise ValueError('unreviewed write in table range')
        lines.append(f'    (0x{request:02x}, 0x{register:04x}, 0x{value:04x}),')
    lines.append('];')
    with args.output.open('x', encoding='utf-8', newline='\n') as output:
        output.write('\n'.join(lines) + '\n')


if __name__ == '__main__':
    main()
