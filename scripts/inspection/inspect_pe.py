"""Record PE imports, exports, transport strings and selected x64 disassembly."""
import argparse
import hashlib
import json
from pathlib import Path
import re

import capstone
import pefile


def inspect(path, regions=()):
    data = path.read_bytes()
    pe = pefile.PE(data=data)
    exports = {e.name.decode(): e.address for e in getattr(getattr(pe, 'DIRECTORY_ENTRY_EXPORT', None), 'symbols', []) if e.name}
    imports = {d.dll.decode(): [i.name.decode() if i.name else i.ordinal for i in d.imports]
               for d in getattr(pe, 'DIRECTORY_ENTRY_IMPORT', [])}
    strings = [s.decode('ascii') for s in re.findall(rb'[ -~]{8,}', data)]
    strings += [s.decode('utf-16le') for s in re.findall(rb'(?:[ -~]\x00){8,}', data)]
    selected = sorted({s for s in strings if re.search(r'CyUSB|CyIoctl|CCyBulk|CCyUSB|DeviceIoControl|WinUsb|\.pdb$', s, re.I)})
    disassembly = {}
    region_disassembly = {}
    if pe.FILE_HEADER.Machine == 0x8664:
        decoder = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
        for start, length in regions:
            region_bytes = pe.get_data(start, length)
            if len(region_bytes) != length:
                raise ValueError('disassembly region extends beyond file data')
            region_disassembly[hex(start)] = [f'{i.address:x}: {i.mnemonic} {i.op_str}'
                                             for i in decoder.disasm(region_bytes, start)]
        for name, address in exports.items():
            if name not in ['ASIGetDataAfterExp', 'ASIGetExpStatus', 'ASIEnableDebugLog',
                            'ASIGetDebugLogIsEnabled', 'ASIGetDebugLogPath']:
                continue
            function = next((e.struct for e in getattr(pe, 'DIRECTORY_ENTRY_EXCEPTION', [])
                             if e.struct.BeginAddress == address), None)
            if function is not None:
                disassembly[name] = [f'{i.address:x}: {i.mnemonic} {i.op_str}' for i in decoder.disasm(
                    pe.get_data(address, function.EndAddress - address), address)]
    return {'file': path.name, 'sha256': hashlib.sha256(data).hexdigest(), 'bytes': len(data),
            'machine': hex(pe.FILE_HEADER.Machine), 'imports': imports,
            'exports': {k: hex(v) for k, v in exports.items()}, 'transportStrings': selected,
            'disassemblyRva': disassembly, 'regionsRva': region_disassembly}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('file', type=Path, nargs='+')
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--region', action='append', default=[], metavar='RVA:LENGTH',
                        help='explicit bounded disassembly, including chained unwind fragments')
    args = parser.parse_args()
    try:
        regions = [tuple(int(part, 0) for part in value.split(':')) for value in args.region]
        if len(regions) > 16 or any(len(r) != 2 or r[0] < 0 or not 0 < r[1] <= 16384 for r in regions):
            raise ValueError()
    except ValueError:
        parser.error('regions must be RVA:LENGTH, with length 1..16384 and at most 16 regions')
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open('x', encoding='utf-8') as output:
        json.dump([inspect(path, regions) for path in args.file], output, indent=2)
