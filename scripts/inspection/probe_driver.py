"""Read standard USB descriptors through the existing ZWO driver, without loading ASI SDK.

Uses only a device path previously observed in our own trace. No vendor requests,
bulk reads, resets, configuration changes, or driver installation are performed.
"""
import argparse
import ctypes as C
from ctypes import wintypes as W
import json
from pathlib import Path
import struct
import subprocess
import sys


def probe(trace):
    paths = {r['path'] for line in trace.read_text().splitlines()
             if (r := json.loads(line))['kind'] == 'device-open'}
    if len(paths) != 1:
        raise ValueError('exactly one camera path must have been observed')
    path = paths.pop()
    if not path.lower().startswith('\\\\?\\usb#vid_03c3&'):
        raise ValueError('not an observed ZWO USB interface')
    k = C.WinDLL('kernel32', use_last_error=True)
    k.CreateFileW.argtypes = [W.LPCWSTR, W.DWORD, W.DWORD, C.c_void_p, W.DWORD, W.DWORD, W.HANDLE]
    k.CreateFileW.restype = W.HANDLE
    k.CloseHandle.argtypes = [W.HANDLE]
    k.CloseHandle.restype = W.BOOL
    k.DeviceIoControl.argtypes = [W.HANDLE, W.DWORD, C.c_void_p, W.DWORD, C.c_void_p,
                                  W.DWORD, C.POINTER(W.DWORD), C.c_void_p]
    k.DeviceIoControl.restype = W.BOOL
    handle = k.CreateFileW(path, 0xc0000000, 3, None, 3, 0, None)
    if handle == C.c_void_p(-1).value:
        raise C.WinError(C.get_last_error())
    try:
        def ioctl(code, data):
            buffer = C.create_string_buffer(data, len(data))
            count = W.DWORD()
            if not k.DeviceIoControl(handle, code, buffer, len(data), buffer, len(data), C.byref(count), None):
                raise C.WinError(C.get_last_error())
            if count.value > len(data):
                raise RuntimeError('invalid driver byte count')
            return buffer.raw[:count.value]

        def descriptor(kind, size):
            # Packed Cypress SINGLE_TRANSFER: 12-byte setup, reserved/endpoint,
            # six ULONG fields. This 38-byte ABI was observed in the ASI trace.
            request = struct.pack('<BBHHHIBBIIIIII', 0x80, 6, kind << 8, 0, size, 5,
                                  0, 0, 0, 0, 0, 0, 38, size) + bytes(size)
            result = ioctl(0x220020, request)
            if len(result) < 38:
                raise RuntimeError('truncated transfer header')
            nt, usb = struct.unpack_from('<II', result, 14)
            offset, length = struct.unpack_from('<II', result, 30)
            if nt or usb or offset != 38 or length != size or len(result) != 38 + size:
                raise RuntimeError(f'descriptor failed: NT={nt:x}, USB={usb:x}, size={len(result)}')
            return result[38:]

        version = struct.unpack('<I', ioctl(0x220000, bytes(4)))[0]
        device = descriptor(1, 18)
        prefix = descriptor(2, 9)
        total = struct.unpack_from('<H', prefix, 2)[0]
        if not 9 <= total <= 4096:
            raise RuntimeError('invalid configuration descriptor length')
        config = descriptor(2, total)
        endpoints = []
        offset = 0
        while offset < len(config):
            length = config[offset]
            if length < 2 or offset + length > len(config):
                raise RuntimeError('invalid descriptor chain')
            desc = config[offset:offset + length]
            if desc[1] == 5 and length >= 7:
                endpoints.append({'address': hex(desc[2]), 'attributes': desc[3],
                                  'maxPacketSize': struct.unpack_from('<H', desc, 4)[0]})
            if desc[1] == 48 and length >= 6 and endpoints:
                endpoints[-1]['maxBurst'] = desc[2]
            offset += length
        return {'sdkLoaded': False, 'driverVersionRaw': f'0x{version:08x}',
                'usbVersionBcd': hex(struct.unpack_from('<H', device, 2)[0]),
                'vendorId': hex(struct.unpack_from('<H', device, 8)[0]),
                'productId': hex(struct.unpack_from('<H', device, 10)[0]),
                'deviceReleaseBcd': hex(struct.unpack_from('<H', device, 12)[0]),
                'configurationBytes': total, 'endpoints': endpoints}
    finally:
        k.CloseHandle(handle)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('trace', type=Path)
    parser.add_argument('--worker', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.worker:
        print(json.dumps(probe(args.trace), indent=2))
    else:
        # Process lifetime bounds even a kernel request that ignores its timeout.
        subprocess.run([sys.executable, __file__, str(args.trace), '--worker'], check=True, timeout=30)
