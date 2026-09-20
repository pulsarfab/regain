"""Windows CAA reference experiments. Requires clearance for eight degrees.

Uses raw HID reports so firmware limits can be distinguished from SDK checks.
Each motion is within two reported degrees and has a five-second stop deadline.
Reference writes do not request movement. Raw evidence remains local.
Power tests require a real, user-performed USB power disconnection.
"""
import argparse
import ctypes as C
from ctypes import wintypes as W
import hashlib
import json
from pathlib import Path
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]
SDK = ROOT / 'CAA_Windows_SDK_V1.5.9/lib/x64/Release/CAA_SRC.dll'


class Hid:
    def __init__(self, record):
        self.record = record
        devices = json.loads(subprocess.check_output(
            [str(ROOT / 'target/release/regain-caa.exe'), 'list'], text=True))
        assert len(devices) == 1, 'exactly one CAA required'
        self.k = C.WinDLL('kernel32', use_last_error=True)
        self.h = C.WinDLL('hid', use_last_error=True)
        self.k.CreateFileW.argtypes = [W.LPCWSTR, W.DWORD, W.DWORD, C.c_void_p, W.DWORD, W.DWORD, W.HANDLE]
        self.k.CreateFileW.restype = W.HANDLE
        self.k.CloseHandle.argtypes = [W.HANDLE]
        for name in ('HidD_SetOutputReport', 'HidD_GetInputReport'):
            fn = getattr(self.h, name)
            fn.argtypes = [W.HANDLE, C.c_void_p, W.ULONG]
            fn.restype = C.c_ubyte
        self.handle = self.k.CreateFileW(devices[0]['path'], 0xc0000000, 0, None, 3, 0, None)
        if self.handle == C.c_void_p(-1).value:
            raise C.WinError(C.get_last_error())
        # Refuse a different report layout before sending experimental commands.
        preparsed = C.c_void_p()
        self.h.HidD_GetPreparsedData.argtypes = [W.HANDLE, C.POINTER(C.c_void_p)]
        self.h.HidD_GetPreparsedData.restype = C.c_ubyte
        self.h.HidP_GetCaps.argtypes = [C.c_void_p, C.c_void_p]
        self.h.HidP_GetCaps.restype = C.c_long
        self.h.HidD_FreePreparsedData.argtypes = [C.c_void_p]
        try:
            assert self.h.HidD_GetPreparsedData(self.handle, C.byref(preparsed))
            caps = (C.c_ushort * 32)()
            assert self.h.HidP_GetCaps(preparsed, caps) == 0x110000
            assert caps[2] == 16 and caps[3] == 16, 'requires inspected 16-byte CAA reports'
        except BaseException:
            self.close()
            raise
        finally:
            if preparsed.value:
                self.h.HidD_FreePreparsedData(preparsed)

    def close(self):
        if self.handle is not None:
            self.k.CloseHandle(self.handle)
            self.handle = None

    def output(self, report, settle=True):
        assert len(report) == 16 and report[:3] == [3, 0x7e, 0x5a]
        buffer = (C.c_ubyte * 16)(*report)
        ok = self.h.HidD_SetOutputReport(self.handle, buffer, 16)
        error = C.get_last_error()
        self.record(kind='hid-output', bytes=report, success=bool(ok))
        if settle:
            time.sleep(.2)
        if not ok:
            raise C.WinError(error)

    def query(self, selector):
        self.output([3, 0x7e, 0x5a, 2, selector] + [0] * 11, settle=selector not in (3, 8))
        buffer = (C.c_ubyte * 16)(1)
        if not self.h.HidD_GetInputReport(self.handle, buffer, 16):
            raise C.WinError(C.get_last_error())
        reply = bytes(buffer)
        self.record(kind='hid-input', bytes=list(reply))
        assert reply[:4] == bytes([1, 0x7e, 0x5a, selector]), reply.hex()
        return reply

    def fingerprint(self):
        info = self.query(4)
        assert info[4:7] == bytes([1, 1, 1]) and info[8:16].rstrip(b'\0') == b'CAA-M54', \
            'reference workup currently requires CAA-M54 firmware 1.1.1'
        return hashlib.sha256(self.query(12)[4:12]).hexdigest()

    def status(self, label=None):
        r = self.query(3)
        value = dict(state=r[4], direction=r[5], degrees=int.from_bytes(r[6:10], 'big') / 10000,
                     limit=int.from_bytes(r[13:15], 'big'), error=r[15])
        self.record(kind='status', label=label, **value)
        return value

    def idle(self):
        s = self.status()
        assert s['state'] == 0 and s['error'] == 0, s
        return s

    def reference(self, degrees):
        assert 0 <= degrees <= 360
        s = self.idle()
        r = [3, 0x7e, 0x5a, 3, 0, s['direction']] + list(round(degrees * 10000).to_bytes(4, 'big'))
        r += [1, 0, 0, 0] + list(s['limit'].to_bytes(2, 'big'))
        self.output(r)
        after = self.status('after-reference-write')
        assert after['state'] == 0 and after['error'] == 0, after
        return after

    def limit(self, degrees):
        assert 1 <= degrees <= 361
        s = self.idle()
        self.output([3, 0x7e, 0x5a, 3, 0, s['direction'], 0x0b, 0xb8, 0, 0, 2, 0, 0, 0]
                    + list(degrees.to_bytes(2, 'big')))
        return self.status('after-limit-write')

    def stop(self):
        self.output([3, 0x7e, 0x5a, 3, 2] + [0] * 11)

    def move(self, target, probe=False):
        s = self.idle()
        assert 0 <= target <= 361 and abs(target - s['degrees']) <= 2.001, (s, target)
        r = [3, 0x7e, 0x5a, 3, 1, s['direction']] + list(round(target * 10000).to_bytes(4, 'big'))
        r += [0, 0, 0, 0] + list(s['limit'].to_bytes(2, 'big'))
        deadline = time.monotonic() + 5
        try:
            self.output(r)
            while time.monotonic() < deadline:
                after = self.status()
                if (after['degrees'] - s['degrees']) * (target - s['degrees']) < -.05:
                    self.stop()
                    stopped = self.idle()
                    self.record(kind='opposite-motion-stopped', target=target, before=s, after=stopped)
                    if probe:
                        return stopped
                    raise RuntimeError('motion went in the opposite direction')
                if abs(after['degrees'] - s['degrees']) > 2.2:
                    raise RuntimeError('unexpected position jump; stopping')
                if after['state'] == 0:
                    self.record(kind='motion-result', target=target, before=s, after=after)
                    return after
                time.sleep(.025)
            raise TimeoutError('bounded motion deadline')
        except BaseException:
            self.stop()
            raise


def main():
    if not __debug__:
        raise SystemExit('Do not use Python -O: this hardware probe requires assertions')
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('phase', choices=['assign', 'sdk-zero', 'boundary', 'prepare-power', 'finish-power'])
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--state', type=Path, default=ROOT / 'artifacts/caa-reference-power-state.json')
    args = p.parse_args()
    with args.output.open('x', encoding='utf-8') as log:
        def record(**value):
            log.write(json.dumps(dict(time=time.time(), **value)) + '\n')
            log.flush()
            if value['kind'] not in ('hid-input', 'hid-output'):
                print(json.dumps(value), flush=True)
        d = Hid(record)
        try:
            fingerprint = d.fingerprint()
            initial = d.idle()
            record(kind='initial', phase=args.phase, status=initial)
            if args.phase == 'assign':
                try:
                    after = d.reference(initial['degrees'] + 1)
                    assert abs(after['degrees'] - initial['degrees'] - 1) < .03, after
                    d.close(); d = Hid(record)
                    assert abs(d.status('reopened')['degrees'] - after['degrees']) < .03
                finally:
                    restored = d.reference(initial['degrees'])
                    assert abs(restored['degrees'] - initial['degrees']) < .03, restored
            elif args.phase == 'sdk-zero':
                assert hashlib.sha256(SDK.read_bytes()).hexdigest() == '413d629adfd81150962211d2241aec2a99420ba2d65c0161f7700da32d534152'
                d.close()
                sdk = C.CDLL(str(SDK))
                sdk.CAACurDegree.argtypes = [C.c_int, C.c_float]
                sdk.CAAMoveToMechanical.argtypes = [C.c_int, C.c_float]
                device = C.c_int()
                assert sdk.CAAGetNum() == 1
                assert sdk.CAAGetID(0, C.byref(device)) == 0
                assert sdk.CAAOpen(device) == 0
                try:
                    for target in (-1.0, 361.0):
                        code = sdk.CAAMoveToMechanical(device, target)
                        record(kind='sdk-range-check', target=target, code=code)
                        assert code == 10
                    code = sdk.CAACurDegree(device, 0.0)
                    record(kind='sdk-zero', code=code)
                    assert code == 0
                    angle = C.c_float()
                    assert sdk.CAAGetDegree(device, C.byref(angle)) == 0
                    record(kind='sdk-angle-after-zero', degrees=angle.value)
                    assert angle.value == 0
                finally:
                    assert sdk.CAAClose(device) == 0
                    d = Hid(record)
                    observed = d.status('native-after-sdk-close')
                    restored = d.reference(initial['degrees'])
                    assert abs(restored['degrees'] - initial['degrees']) < .03, restored
                assert observed['degrees'] == 0, observed
            elif args.phase == 'boundary':
                # Re-label the current location near 360, keeping motion small.
                # Track displacement separately across reference changes.
                assert 6 <= initial['degrees'] <= min(354, initial['limit'] - 6), \
                    'requires six degrees of room around the original reference'
                physical_delta = 0.0
                reference = initial['degrees']
                try:
                    assert abs(d.reference(359)['degrees'] - 359) < .03
                    reference = 359.0
                    for limit, target in ((360, 360), (360, 361), (361, 361)):
                        if limit == 361:
                            assert d.reference(360)['degrees'] == 360
                            reference = 360
                        s = d.limit(limit)
                        record(kind='limit-observation', requested=limit, reported=s['limit'])
                        before = d.idle()['degrees']
                        after = d.move(target, probe=True)
                        physical_delta += after['degrees'] - before
                        reference = after['degrees']
                        if after['error']:
                            record(kind='firmware-rejection', status=after)
                            break
                    # Only continue if firmware left a healthy, interpretable state.
                    d.idle()
                    assert d.reference(0)['degrees'] == 0
                    reference = 0
                    d.limit(initial['limit'])
                    after = d.move(1)
                    physical_delta += after['degrees']
                    reference = after['degrees']
                    record(kind='move-after-zero-reset', status=after, physicalDelta=physical_delta)
                finally:
                    d.stop()
                    current = d.idle()['degrees']
                    physical_delta += current - reference
                    assert abs(physical_delta) <= 6, 'manual position reconciliation needed'
                    # Restore the original coordinate system at the current location.
                    restored_angle = initial['degrees'] + physical_delta
                    assert abs(d.reference(restored_angle)['degrees'] - restored_angle) < .03
                    d.limit(initial['limit'])
                    # Small individual return steps, never wrap around the origin.
                    while abs(d.idle()['degrees'] - initial['degrees']) > .03:
                        current = d.idle()['degrees']
                        target = current + max(-2, min(2, initial['degrees'] - current))
                        after = d.move(target)
                        assert abs(after['degrees'] - target) < .03, after
                    record(kind='restored', status=d.idle())
            elif args.phase == 'prepare-power':
                assert not args.state.exists(), 'existing power-test state; finish it first'
                value = dict(initial=initial, marker=initial['degrees'] + 1.25, fingerprint=fingerprint)
                assert value['marker'] <= 360
                # Save recovery information before issuing a reference write.
                args.state.write_text(json.dumps(value, indent=2), encoding='utf-8')
                assert abs(d.reference(value['marker'])['degrees'] - value['marker']) < .03
                record(kind='awaiting-physical-power-cycle', marker=value['marker'])
            else:
                value = json.loads(args.state.read_text(encoding='utf-8'))
                assert not value.get('completed'), 'power test already finished; use a new state file'
                assert not value.get('cancelled'), 'power test was cancelled; use a new state file'
                assert value['fingerprint'] == fingerprint, 'different CAA; leave its reference unchanged'
                record(kind='after-physical-power-cycle', observed=initial, expected=value['marker'],
                       retained=abs(initial['degrees'] - value['marker']) < .03)
                restored = d.reference(value['initial']['degrees'])
                assert abs(restored['degrees'] - value['initial']['degrees']) < .03
                value['completed'] = True
                value['observed'] = initial
                args.state.write_text(json.dumps(value, indent=2), encoding='utf-8')
            record(kind='complete', phase=args.phase)
        finally:
            d.close()


if __name__ == '__main__':
    main()
