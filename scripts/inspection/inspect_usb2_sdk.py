"""Static USB2 spike: inspect the pinned Linux SDK without loading it or opening USB.

Requires capstone for --disassemble. Output is research evidence, not a driver.
Only this reviewed ELF64 x86-64 binary/layout is accepted.
"""
import argparse
import hashlib
from pathlib import Path
import re
import struct

SHA256 = "d1de4a5ab85c8cafbddfad9c593bbba515890d3adf20c1ca44dafcf15f2775ce"
DEFAULT = Path(__file__).resolve().parents[2] / "vendor/zwo/native/x86_64-unknown-linux-gnu/libASICamera2.so"


def inspect(path, pattern, disassemble=False):
    data = path.read_bytes()
    if hashlib.sha256(data).hexdigest() != SHA256:
        raise ValueError("SDK digest differs from the reviewed 1.41 Linux x86-64 binary")
    offset = struct.unpack_from("<Q", data, 40)[0]
    stride, count, names_index = struct.unpack_from("<HHH", data, 58)
    sections = [struct.unpack_from("<IIQQQQIIQQ", data, offset + i * stride) for i in range(count)]

    def section_bytes(section):
        return data[section[4]:section[4] + section[5]]

    def string(table, index):
        return table[index:].split(b"\0", 1)[0].decode("utf-8")

    names = section_bytes(sections[names_index])
    named = {string(names, s[0]): s for s in sections}
    symbols = []
    tables = {}
    for number, section in enumerate(sections):
        if section[1] not in (2, 11):
            continue
        strings = section_bytes(sections[section[6]])
        table = []
        for pos in range(section[4], section[4] + section[5], section[9]):
            name, info, _, index, address, size = struct.unpack_from("<IBBHQQ", data, pos)
            symbol = (string(strings, name), info & 15, index, address, size)
            table.append(symbol)
            symbols.append(symbol)
        tables[number] = table
    addresses = {s[3]: s[0] for s in symbols if s[1] == 2 and s[3]}
    # The digest pins the conventional x86-64 .plt/.rela.plt layout used here.
    plt, relocations = named[".plt"], named[".rela.plt"]
    for i, pos in enumerate(range(relocations[4], relocations[4] + relocations[5], relocations[9])):
        _, info, _ = struct.unpack_from("<QQq", data, pos)
        addresses[plt[3] + 16 * (i + 1)] = tables[relocations[6]][info >> 32][0] + "@plt"
    selected = sorted({s for s in symbols if s[1] == 2 and s[3] and re.search(pattern, s[0])}, key=lambda s: s[3])
    if not selected:
        raise ValueError("No matching defined function symbols")
    print(f"SDK SHA256 {SHA256}; {len(selected)} matching symbols")
    if disassemble:
        import capstone
        decoder = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
        decoder.detail = True
    for name, _, index, address, size in selected:
        print(f"{address:#x} size={size} {name}")
        if not disassemble:
            continue
        section = sections[index]
        start = section[4] + address - section[3]
        for insn in decoder.disasm(data[start:start + size], address):
            label = ""
            if insn.mnemonic in ("call", "jmp") and insn.op_str.startswith("0x"):
                label = addresses.get(int(insn.op_str, 16), "")
            for operand in insn.operands:
                if operand.type != capstone.x86.X86_OP_MEM or operand.mem.base != capstone.x86.X86_REG_RIP:
                    continue
                target = insn.address + insn.size + operand.mem.disp
                for candidate in sections:
                    if candidate[1] == 1 and candidate[3] <= target and target + 4 <= candidate[3] + candidate[5]:
                        pos = candidate[4] + target - candidate[3]
                        unsigned = struct.unpack_from("<I", data, pos)[0]
                        floating = struct.unpack_from("<f", data, pos)[0]
                        label += f" [ref={target:#x} u32={unsigned} f32={floating:g}]"
                        break
            print(f"  {insn.address:x}: {insn.mnemonic} {insn.op_str} {label}".rstrip())


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk", type=Path, default=DEFAULT)
    parser.add_argument("--symbol", required=True, help="regex over mangled function symbols")
    parser.add_argument("--disassemble", action="store_true")
    args = parser.parse_args()
    inspect(args.sdk, args.symbol, args.disassemble)
