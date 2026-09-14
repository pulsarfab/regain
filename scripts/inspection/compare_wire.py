"""Compare in-memory USB samples to SDK RAW16; never emit pixel samples."""
import numpy as np


def complete_runs(submitted, chunks, expected):
    """Accept only consecutive successful submissions totaling exactly one frame.

    A missing/cancelled request breaks the run. Never stitch across that gap.
    """
    frames, rejected = [], []
    run, size = [], 0
    for sequence in submitted:
        chunk = chunks.get(sequence)
        if chunk is None or size + len(chunk) > expected:
            if size:
                rejected.append(size)
            run, size = [], 0
        if chunk is None:
            continue
        if len(chunk) > expected:
            rejected.append(len(chunk))
            continue
        run.append(chunk)
        size += len(chunk)
        if size == expected:
            frames.append(b''.join(run))
            run, size = [], 0
    if size:
        rejected.append(size)
    return frames, rejected


def compare(wire, frame, width, height):
    expected = width * height * 2
    if len(wire) != expected or len(frame) != expected:
        return {'wireBytes': len(wire), 'frameBytes': len(frame), 'comparable': False}
    target = np.frombuffer(frame, dtype='<u2').reshape(height, width)
    raw = np.frombuffer(wire, dtype='<u2').reshape(height, width)
    mask = raw != target
    y, x = np.nonzero(mask)
    differences = {'count': int(mask.sum()),
                   'bounds': [int(x.min()), int(y.min()), int(x.max()), int(y.max())] if len(x) else None,
                   'wireLow4Zero': bool(np.all((raw & 15) == 0)),
                   'sdkChangedDown': int(np.count_nonzero(target[mask] < raw[mask])),
                   'sdkChangedUp': int(np.count_nonzero(target[mask] > raw[mask]))}
    results = []
    for endian in ['<', '>']:
        source = np.frombuffer(wire, dtype=endian + 'u2').astype(np.uint16).reshape(height, width)
        for shift in [0, 4, -4, 2, -2, 8, -8]:
            candidate = source if shift == 0 else (source << shift if shift > 0 else source >> -shift)
            matched = int(np.count_nonzero(candidate == target))
            results.append({'endian': endian, 'shift': shift, 'matchedPixels': matched,
                            'totalPixels': target.size, 'exact': matched == target.size})
    return {'wireBytes': len(wire), 'frameBytes': len(frame), 'comparable': True,
            'sdkLow4Zero': bool(np.all((target & 15) == 0)),
            'differences': differences,
            'candidates': sorted(results, key=lambda r: r['matchedPixels'], reverse=True)}
