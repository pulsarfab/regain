"""Summarize transport metadata without publishing device paths or frame hashes."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import struct


def summarize(path):
    rows = [json.loads(line) for line in path.read_text().splitlines()]
    submitted = {r['sequence']: r for r in rows if r['kind'] == 'io-submit'}
    starts = [r['timeMs'] for r in rows if r['kind'] == 'sdk-enter' and r['name'] == 'ASIStartExposure']
    frames = []
    for index, start in enumerate(starts):
        end = starts[index + 1] if index + 1 < len(starts) else float('inf')
        group = [r for r in rows if start <= r.get('timeMs', -1) < end]
        bulk = [r for r in group if r['kind'] == 'io-submit' and r['code'] == '0x22004b']
        ids = {r['sequence'] for r in bulk}
        completed = [r for r in group if r['kind'] in ['io-return', 'io-complete']
                     and r['sequence'] in ids and (r['ok'] or r['lastError'] not in [996, 997])]
        successes = [r for r in completed if r['ok']]
        errors = Counter()
        for r in completed:
            if not r['ok']:
                header = bytes.fromhex(r.get('header') or '')
                nt, usb = struct.unpack_from('<II', header, 14) if len(header) == 38 else (0, 0)
                errors[f"win32={r['lastError']} nt=0x{nt:08x} usb=0x{usb:08x}"] += 1
        ready = next((r['timeMs'] for r in group if r['kind'] == 'sdk-leave'
                      and r['name'] == 'ASIGetExpStatus' and r.get('status') == 2), None)
        downloads = [r for r in group if r['kind'] == 'sdk-enter' and r['name'] == 'ASIGetDataAfterExp']
        download = downloads[0]['timeMs'] if downloads else None
        download_end = next((r['timeMs'] for r in group if r['kind'] == 'sdk-leave'
                             and r['name'] == 'ASIGetDataAfterExp'), None)
        hashes = [r for r in rows if r['kind'] == 'bulk-hash' and r['sequence'] in ids]
        hash_counts = Counter(r['sha256'] for r in hashes)
        frames.append({
            'attempt': index + 1, 'submittedBulkRequests': len(bulk),
            'bulkRequestSizes': dict(Counter(r['outputLength'] for r in bulk)),
            'successfulBulkRequests': len(successes),
            'successfulBulkBytes': sum(r['bytes'] or 0 for r in successes),
            'completionErrors': dict(errors),
            'bulkHashesCollected': len(hashes),
            'distinctBulkHashes': len(hash_counts),
            'duplicateBulkHashGroups': sum(count > 1 for count in hash_counts.values()),
            'firstBulkSubmitMs': min((r['timeMs'] - start for r in bulk), default=None),
            'lastSuccessfulBulkMs': max((r['timeMs'] - start for r in successes), default=None),
            'readyMs': ready - start if ready is not None else None,
            'downloadMs': download - start if download is not None else None,
            'ioSubmitsDuringDownload': sum(r['kind'] == 'io-submit'
                                           and download <= r.get('timeMs', 0) <= download_end
                                           for r in group) if download_end is not None else None,
            'downloadDurationsMs': [r['elapsedMs'] for r in group if r['kind'] == 'sdk-leave'
                                    and r['name'] == 'ASIGetDataAfterExp'],
            'states': sorted({r['status'] for r in group if r['kind'] == 'sdk-leave'
                              and r['name'] == 'ASIGetExpStatus' and r.get('status') is not None}),
        })
    return {'trace': path.name, 'traceSha256': hashlib.sha256(path.read_bytes()).hexdigest(),
            'events': dict(Counter(r['kind'] for r in rows)),
            'postDownloadStatuses': [r['status'] for r in rows if r['kind'] == 'post-download-status'],
            'injectedCancellations': [{'ok': r['ok'], 'lastError': r['lastError']} for r in rows
                                      if r['kind'] == 'injected-cancel'],
            'ioctlCounts': dict(Counter(r['code'] for r in submitted.values())), 'attempts': frames}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('trace', type=Path, nargs='+')
    args = parser.parse_args()
    print(json.dumps([summarize(path) for path in args.trace], indent=2))
