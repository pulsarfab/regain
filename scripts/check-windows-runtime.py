"""Reject VC++ redistributable imports in Windows payloads, without loading them.

Uses only the Python standard library. Windows' own msvcrt/UCRT and .NET remain
OS/framework prerequisites; versioned VC++ runtime DLLs must not be required.
Both ordinary and delay-load imports are inspected, including vendor DLLs.
"""
import argparse
import json
from pathlib import Path
import re
import struct


VC_RUNTIME = re.compile(r"(?:vcruntime|msvcp|msvcr|concrt|vcomp)\d[^/\\]*\.dll$", re.I)


def imports(path):
    data = Path(path).read_bytes()

    def unpack(fmt, offset):
        size = struct.calcsize(fmt)
        if offset < 0 or offset + size > len(data):
            raise ValueError("Truncated PE structure")
        return struct.unpack_from(fmt, data, offset)

    if data[:2] != b"MZ":
        raise ValueError("Missing DOS signature")
    pe, = unpack("<I", 0x3c)
    if data[pe:pe + 4] != b"PE\0\0":
        raise ValueError("Missing PE signature")
    sections, = unpack("<H", pe + 6)
    optional_size, = unpack("<H", pe + 20)
    optional = pe + 24
    magic, = unpack("<H", optional)
    if magic == 0x10b:
        directory_offset, count_offset = 96, 92
        image_base, = unpack("<I", optional + 28)
    elif magic == 0x20b:
        directory_offset, count_offset = 112, 108
        image_base, = unpack("<Q", optional + 24)
    else:
        raise ValueError("Unsupported PE optional header")
    count, = unpack("<I", optional + count_offset)
    if sections == 0 or optional_size < directory_offset or count > (optional_size - directory_offset) // 8:
        raise ValueError("Invalid PE header bounds")
    header_size, = unpack("<I", optional + 60)
    ranges = []
    for index in range(sections):
        _, rva, raw_size, raw_offset = unpack("<IIII", optional + optional_size + index * 40 + 8)
        if raw_offset + raw_size > len(data):
            raise ValueError("Truncated PE section")
        ranges.append((rva, raw_size, raw_offset))

    def offset(rva, size):
        if 0 <= rva < header_size and rva + size <= min(header_size, len(data)):
            return rva
        for start, length, raw in ranges:
            if start <= rva and rva + size <= start + length:
                return raw + rva - start
        raise ValueError("PE import points outside file-backed data")

    def name(rva):
        value = bytearray()
        for index in range(260):
            byte = data[offset(rva + index, 1)]
            if byte == 0:
                if not value or any(c < 32 or c > 126 for c in value):
                    raise ValueError("Invalid PE import name")
                return value.decode("ascii")
            value.append(byte)
        raise ValueError("Unterminated PE import name")

    result = set()
    for index, width in ((1, 20), (13, 32)):
        if count <= index:
            continue
        rva, size = unpack("<II", optional + directory_offset + index * 8)
        if rva == 0 and size == 0:
            continue
        if not rva or size < width:
            raise ValueError("Invalid PE import directory")
        for position in range(0, size - width + 1, width):
            fields = unpack("<" + "I" * (width // 4), offset(rva + position, width))
            if not any(fields):
                break
            address = fields[3] if index == 1 else fields[1]
            if index == 13:
                if fields[0] not in (0, 1):
                    raise ValueError("Unknown delay-import attributes")
                if fields[0] == 0:
                    address -= image_base
            result.add(name(address))
        else:
            raise ValueError("Unterminated PE import directory")
    return sorted(result, key=str.lower)


def audit(paths):
    binaries = set()
    for path in map(Path, paths):
        if path.is_dir():
            binaries.update(p for p in path.rglob("*") if p.is_file() and p.suffix.lower() in (".exe", ".dll"))
        elif path.is_file():
            binaries.add(path)
        else:
            raise ValueError(f"Missing Windows payload: {path}")
    if not binaries:
        raise ValueError("No Windows binaries found")
    report = {}
    for binary in sorted(binaries):
        try:
            dependencies = imports(binary)
        except ValueError as error:
            raise ValueError(f"{binary}: {error}") from error
        unexpected = [dll for dll in dependencies if VC_RUNTIME.fullmatch(dll)]
        if unexpected:
            raise ValueError(f"{binary} requires VC++ runtime DLLs: {', '.join(unexpected)}. Rebuild with crt-static; do not ship this payload.")
        report[str(binary)] = dependencies
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("paths", nargs="+", type=Path)
    args = parser.parse_args()
    try:
        print(json.dumps(audit(args.paths), indent=2))
    except ValueError as error:
        parser.exit(1, str(error) + "\n")
