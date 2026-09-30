"""Reproduce the reviewed ASI662MC volatile register tables from the pinned local trace."""
import argparse
import hashlib
import json
from pathlib import Path
import struct

TRACE_SHA256 = '42a48acfbf26c0a311fee2f6754f8561f55cdb38246e78d3748b73201acbd93b'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('trace', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    raw = args.trace.read_bytes()
    if hashlib.sha256(raw).hexdigest() != TRACE_SHA256:
        parser.error('requires the reviewed asi662-full40-20260930 trace; do not replay arbitrary traces')
    events = [json.loads(line) for line in raw.splitlines()]
    lines = ['//! ASI662MC observed volatile sensor/FPGA configuration, SDK 1.41 baseline.',
             '//! Source: asi662-full40-20260930 trace (see docs/asi662mc.md).',
             '//! No calibration, serial, EEPROM, or firmware payloads are embedded.']
    for name, low, high in [('INITIALIZE', 269, 438), ('RAW16_FULL', 561, 636)]:
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
