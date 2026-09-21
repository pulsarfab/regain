"""Compare ASI6200 Camera Kit RAW16 samples with the independent Rust processor.

Reads only local evidence; never opens a camera. Reconstructs calibration from
this run, verifies sample hashes and uses the exact USB sequences associated
with each saved SDK frame. Reports digests/statistics, not calibration/pixels.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('kit', type=Path)
    parser.add_argument('--worker', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    manifest = json.loads((args.kit / 'manifest.json').read_text())
    assert manifest['camera']['name'] == 'ZWO ASI6200MM Pro'
    assert manifest['completed'] and not manifest['errors']
    events = [json.loads(line) for line in (args.kit / 'events.jsonl').read_text().splitlines()]
    reads, blocks, revisions = {}, {}, set()
    for event in events:
        if event['kind'] == 'io-submit' and event['code'] == '0x220020':
            if event.get('usb', {}).get('pid') != '620b':
                continue
            fields = struct.unpack('<BBHHH', bytes.fromhex(event['header'])[:8])
            if fields[0] == 0xc0:
                reads[event['sequence']] = fields
        elif event['kind'] == 'control-payload' and event['sequence'] in reads:
            _, request, register, index, length = reads[event['sequence']]
            payload = bytes.fromhex(event['data'])
            assert len(payload) == length
            if request == 0xc3 and index >= 0x400:
                offset = (index << 8) - 0x40000
                if offset in blocks:
                    assert blocks[offset] == payload, 'calibration changed during run'
                blocks[offset] = payload
            elif request == 0xbc and register == 0x1c:
                revisions.add(payload[0])
    assert len(revisions) == 1 and revisions <= {3, 5}
    calibration = bytearray()
    for offset, payload in sorted(blocks.items()):
        assert offset == len(calibration), 'missing calibration block'
        calibration.extend(payload)
    assert calibration[:4] == b'ASID'
    length = int.from_bytes(calibration[4:8], 'big')
    assert 8 <= length <= len(calibration)
    calibration = calibration[:length]
    samples = {s['sequence']: s for s in json.loads((args.kit / 'sample-index.json').read_text())}
    results = []
    for case in manifest['cases']:
        controls = case['requested']['controls']
        if any(controls.get(str(c), 0) for c in (9, 13, 14)):
            continue
        assert case['outcome'] == 'passed', case['name']
        for frame in case['frames']:
            if not frame['sample']:
                continue
            chunks = []
            for sequence in frame['usbSequences']:
                sample = samples[sequence]
                assert sample['exercise'] == case['name']
                data = (args.kit / sample['file']).read_bytes()
                assert len(data) == sample['bytes']
                assert hashlib.sha256(data).hexdigest() == sample['sha256']
                chunks.append(data)
            wire = b''.join(chunks)
            exposure = case['requested']['exposure']
            assert len(wire) == exposure['width'] * exposure['height'] * exposure['bin'] ** 2 * 2
            sdk = (args.kit / frame['sample']).read_bytes()
            assert hashlib.sha256(sdk).hexdigest() == frame['sha256']
            header = json.dumps(dict(exposure, model='asi6200mm-pro', calibrationBytes=len(calibration))).encode()
            run = subprocess.run([str(args.worker.resolve()), 'zwo', 'camera-direct', '--process-frame'],
                                 input=struct.pack('<I', len(header)) + header + calibration + wire,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30, check=True)
            size = struct.unpack('<I', run.stdout[:4])[0]
            metadata = json.loads(run.stdout[4:4+size])
            direct = run.stdout[4+size:]
            assert direct == sdk, case['name']
            results.append(dict(case=case['name'], bin=exposure['bin'], exact=True,
                                sha256=hashlib.sha256(direct).hexdigest(), **metadata))
    assert results, 'no normal RAW16 samples compared'
    report = dict(hardwareRevision=next(iter(revisions)),
                  workerSha256=hashlib.sha256(args.worker.read_bytes()).hexdigest(),
                  sdkSha256=manifest['sdkSha256'], cases=results)
    with args.output.open('x', encoding='utf-8') as output:
        json.dump(report, output, indent=2)
        output.write('\n')
    print(f'{len(results)} samples match every SDK byte; revision {report["hardwareRevision"]}')


if __name__ == '__main__':
    main()
