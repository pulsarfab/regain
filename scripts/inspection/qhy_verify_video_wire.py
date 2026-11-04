"""Compare every SDK live image hash with full-sensor QHY600 USB wire frames."""
import argparse
import array
import hashlib
import json
import mmap
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    rows = [json.loads(line) for line in (args.directory/'sdk.jsonl').read_text(encoding='utf-8').splitlines()]
    sdk = [row for row in rows if 'liveFrame' in row]
    assert sdk and all((r['width'],r['height'],r['bits']) == (9600,6422,sdk[0]['bits']) for r in sdk)
    bits = sdk[0]['bits']
    assert bits in (8,16)
    width,height,extra = 9600,6422,34
    stride = width*(bits//8)
    size = (height+extra)*stride
    marker = bytes.fromhex('ee11dd22ee11dd22' if bits==8 else '55554444ee11dd22')
    footer = marker if bits==8 else bytes.fromhex('ee11dd2255554444')
    hashes = []
    with (args.directory/'wire.bin').open('rb') as file, mmap.mmap(file.fileno(),0,access=mmap.ACCESS_READ) as wire:
        pos = 0
        while True:
            pos = wire.find(marker,pos)
            if pos < 0:
                break
            first = pos+8
            end = first+size
            if wire[end:end+8] == footer:
                pixels = wire[first+extra*stride:end]
                if bits==16:
                    # Worker output is little-endian; the traced wire is big-endian.
                    words = array.array('H',pixels)
                    words.byteswap()
                    pixels = words.tobytes()
                hashes.append(hashlib.sha256(pixels).hexdigest())
            pos += 8
    expected = [r['sha256'] for r in sdk]
    # Extra wire frames may be drained but not returned by the SDK. Every SDK
    # frame must appear, in order, with its complete pixel hash unchanged.
    cursor = 0
    for digest in expected:
        cursor = hashes.index(digest,cursor)+1
    assert len(set(expected)) == len(expected), 'SDK delivered a duplicate image'
    result = dict(sdkFrames=len(sdk),matched=len(expected),completeWireFrames=len(hashes),
                  width=width,height=height,format=f'RAW{bits}',leadingRowsRemoved=extra,
                  bigEndianWire=bits==16,sharedFrameDelimiter=bits==8)
    (args.directory/'wire-verification.json').write_text(json.dumps(result,indent=2),encoding='utf-8')
    print(json.dumps(result))


if __name__ == '__main__':
    main()
