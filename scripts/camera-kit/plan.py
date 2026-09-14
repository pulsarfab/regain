"""Bounded, one-variable-at-a-time RAW16 exercises derived from SDK capabilities."""
MAX_FRAME = 128 * 1024 * 1024
CONTROL_NAMES = {0: 'gain', 1: 'exposure', 5: 'offset', 6: 'usb-bandwidth',
                 9: 'flip', 13: 'hardware-bin', 14: 'high-speed', 18: 'mono-bin', 20: 'pattern-adjust'}
# No reset, firmware, GPS, auto-controller, cooler or heater writes.
IMAGING_CONTROLS = (0, 1, 5, 6, 9, 13, 14, 18, 20)


def make_plan(camera, controls, profile):
    caps = {c['type']: c for c in controls}
    width, height = int(camera['width']), int(camera['height'])
    if 2 not in camera['formats'] or width < 64 or height < 64 or 1 not in camera['bins']:
        raise ValueError('This kit requires RAW16, bin 1 and a sensor at least 64 x 64.')
    exposure = caps.get(1)
    if not exposure:
        raise ValueError('SDK did not provide an exposure control.')
    baseline = {c: caps[c]['value'] for c in IMAGING_CONTROLS if c != 1 and c in caps and caps[c]['writable']}
    for c, value in {6: 40, 9: 0, 13: 0, 14: 0, 18: 0, 20: 0}.items():
        if c in baseline and caps[c]['min'] <= value <= caps[c]['max']:
            baseline[c] = value
    duration = max(exposure['min'], min(100000, exposure['max']))
    w, h = min(width, 512) // 8 * 8, min(height, 256) // 2 * 2
    default = dict(width=w, height=h, bin=1, x=0, y=0, microseconds=duration, dark=True)
    cases, skipped = [], []

    def add(name, parameters=None, values=None, frames=1, samples=True):
        request = {**default, **(parameters or {})}
        settings = {**baseline, **(values or {})}
        reason = None
        if not exposure['min'] <= request['microseconds'] <= exposure['max']:
            reason = 'outside SDK exposure range'
        elif request['width'] * request['height'] * 2 > MAX_FRAME:
            reason = 'exceeds 128 MiB frame limit'
        elif (request['width'] < 8 or request['height'] < 2
              or (request['x'] + request['width']) * request['bin'] > width
              or (request['y'] + request['height']) * request['bin'] > height):
            reason = 'outside sensor bounds'
        elif any(c not in caps or not caps[c]['writable'] or not caps[c]['min'] <= v <= caps[c]['max']
                 for c, v in settings.items()):
            reason = 'outside SDK control range'
        if reason:
            skipped.append(dict(name=name, reason=reason))
        else:
            cases.append(dict(name=name, exposure=request, controls=settings, frames=frames, samples=samples))

    add('baseline-repeat', frames=2, samples=True)
    add('full-frame', dict(width=width // 8 * 8, height=height // 2 * 2), samples=False)
    add('roi-64', dict(width=64, height=64))
    if width >= w + 16 and height >= h + 2:
        add('roi-moved', dict(x=16, y=2))
        add('roi-far-edge', dict(x=(width - w) // 16 * 16, y=(height - h) // 2 * 2))
    for b in sorted(set(camera['bins'])):
        if 1 < b <= 16:
            add(f'bin-{b}', dict(bin=b, width=min(w, width // b) // 8 * 8,
                                height=min(h, height // b) // 2 * 2))
    times = [1000, 990000, 1010000, 2000000]
    if profile == 'extended':
        times += [32, 100, 10000, 490000, 500000, 510000, 999000, 1000000, 1001000, 10000000, 30000000, 60000000]
    for us in sorted(set(times)):
        add(f'exposure-{us}us', dict(microseconds=us))
    for c in (0, 5, 6):
        if c not in baseline:
            skipped.append(dict(name=CONTROL_NAMES[c], reason='control is not writable'))
            continue
        low, high = caps[c]['min'], caps[c]['max']
        values = {low, (low + high) // 2, high}
        if profile == 'extended' and c == 0:
            # Candidate conversion boundaries, never assumed valid for a new model.
            values.update(v for v in (99, 100, 101, 179, 180, 181, 249, 250, 251, 349, 350, 351) if low <= v <= high)
        for value in sorted(values - {baseline[c]}):
            add(f'{CONTROL_NAMES[c]}-{value}', values={c: value})
    if profile == 'extended':
        for c in (9, 13, 14, 18, 20):
            if c in baseline and caps[c]['min'] <= 1 <= caps[c]['max']:
                add(f'{CONTROL_NAMES[c]}-on', values={c: 1})
    add('baseline-after-sweeps')
    return cases, skipped
