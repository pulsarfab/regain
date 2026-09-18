"""Exercise 450 degrees of CAA travel, then unwind, using 90-degree moves.

Requires clearance and cable slack for 450 degrees in the positive direction.
Mechanical zero is reset between forward moves. Reverse moves restore the
starting orientation and reference. This is an explicit hardware experiment,
not normal cable-limit handling. Requires the compiled native CAA enumerator.
"""
import argparse
import json
from pathlib import Path
import time

from validate_caa_reference import Hid


def move_leg(device, target, record, direction, index):
    before = device.idle()
    start = before['degrees']
    assert 0 <= target <= before['limit'] <= 360
    assert 0 < abs(target - start) <= 90.03
    sign = 1 if target > start else -1
    report = [3, 0x7e, 0x5a, 3, 1, before['direction']]
    report += list(round(target * 10000).to_bytes(4, 'big'))
    report += [0, 0, 0, 0] + list(before['limit'].to_bytes(2, 'big'))
    record(kind='leg-start', direction=direction, index=index, start=start, target=target)
    deadline = time.monotonic() + 30
    previous = start
    try:
        device.output(report)
        while time.monotonic() < deadline:
            current = device.status()
            position = current['degrees']
            assert current['error'] == 0, current
            assert min(start, target) - .05 <= position <= max(start, target) + .05, current
            assert (position - previous) * sign >= -.05, 'motion reversed unexpectedly'
            if current['state'] == 0:
                assert abs(position - target) <= .03, 'stopped before reaching target'
                record(kind='leg-complete', direction=direction, index=index,
                       start=start, target=target, reached=position, displacement=position-start)
                return position - start
            previous = position
            time.sleep(.1)
        raise TimeoutError('CAA quarter-turn exceeded 30 seconds')
    except BaseException:
        device.stop()
        raise


def main():
    if not __debug__:
        raise SystemExit('Do not use Python -O: this hardware probe requires assertions')
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--cancel-power-test-state', type=Path,
                   help='restore the pending marker before starting; does not validate power retention')
    args = p.parse_args()
    with args.output.open('x', encoding='utf-8') as log:
        def record(**value):
            log.write(json.dumps(dict(time=time.time(), **value)) + '\n')
            log.flush()
            if value['kind'] not in ('hid-input', 'hid-output', 'status'):
                print(json.dumps(value), flush=True)
        device = Hid(record)
        total = 0.0
        leg_start = None
        try:
            fingerprint = device.fingerprint()
            if args.cancel_power_test_state:
                state = json.loads(args.cancel_power_test_state.read_text(encoding='utf-8'))
                assert state['fingerprint'] == fingerprint
                assert not state.get('completed') and not state.get('cancelled')
                assert abs(device.idle()['degrees'] - state['marker']) < .03, 'power marker changed'
                restored = device.reference(state['initial']['degrees'])
                assert abs(restored['degrees'] - state['initial']['degrees']) < .03
                state['cancelled'] = True
                state['reason'] = 'superseded by requested multi-turn experiment; no confirmed power cycle'
                args.cancel_power_test_state.write_text(json.dumps(state, indent=2), encoding='utf-8')
                record(kind='power-test-cancelled', restored=restored)
            initial = device.idle()
            settings = device.query(8)[4:6]
            assert initial['limit'] >= 90
            record(kind='initial', status=initial, beep=bool(settings[0]), reverse=bool(settings[1]))
            displacements = []
            for index in range(1, 6):
                zero = device.reference(0)
                assert zero['degrees'] == 0
                leg_start = 0.0
                delta = move_leg(device, 90, record, 'forward', index)
                total += delta
                leg_start = None
                displacements.append(delta)
                record(kind='cumulative-travel', direction='forward', index=index, degrees=total)
            assert total > 449.8
            record(kind='past-one-revolution', degrees=total, resets=5)
            # Use the measured forward displacements to undo the same travel.
            for index, delta in enumerate(reversed(displacements), 1):
                reference = device.reference(delta)
                assert abs(reference['degrees'] - delta) < .03
                leg_start = reference['degrees']
                actual = move_leg(device, 0, record, 'return', index)
                total += actual
                leg_start = None
                record(kind='cumulative-travel', direction='return', index=index, degrees=total)
            assert abs(total) <= .1, f'residual reported travel {total}'
            restored = device.reference(initial['degrees'])
            assert abs(restored['degrees'] - initial['degrees']) < .03
            assert restored['limit'] == initial['limit'] and restored['error'] == 0
            assert device.query(8)[4:6] == settings
            record(kind='complete', forwardDegrees=sum(displacements), netReportedDegrees=total,
                   final=restored, settingsRestored=True)
        except BaseException as error:
            # A stall, uncertain write or bad position invalidates automatic
            # unwinding. Stop and retain the last reference for inspection.
            device.stop()
            try:
                status = device.status('after-abort')
                if leg_start is not None:
                    total += status['degrees'] - leg_start
                record(kind='aborted', error=str(error), netReportedDegrees=total, status=status)
            except Exception as status_error:
                record(kind='aborted', error=str(error), statusError=str(status_error),
                       netReportedDegreesBeforeIncompleteLeg=total)
            raise
        finally:
            device.close()


if __name__ == '__main__':
    main()
