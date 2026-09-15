"""Extract reviewed ASI6200MM Pro P25 volatile setup writes from the SDK 1.41 trace."""
import argparse
import hashlib
import json
from pathlib import Path
import struct

TRACE_SHA256 = 'c41f404cf951446f94ed69e4406334db3da89ef74871420722b3f3c2763aaac8'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('trace', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    raw = args.trace.read_bytes()
    if hashlib.sha256(raw).hexdigest() != TRACE_SHA256:
        parser.error('requires the reviewed asi6200-p25-mapped-01 trace')
    events = [json.loads(line) for line in raw.splitlines()]
    lines = ['//! ASI6200MM Pro P25 volatile configuration observed with SDK 1.41.',
             '//! Reproduce with scripts/inspection/extract_asi6200_tables.py.',
             '//! No calibration, serial, EEPROM, or firmware payloads.']
    for name, low, high in [('INITIALIZE', 354, 572), ('RAW16', 678, 790)]:
        lines.append(f'pub const {name}: &[(u8, u16, u16)] = &[')
        for event in events:
            if event['kind'] != 'io-submit' or event['code'] != '0x220020' or not low <= event['sequence'] <= high:
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
