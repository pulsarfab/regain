"""PulsarFab regain camera exercise kit. Collect locally; never upload or replay USB writes."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
from pathlib import Path
import platform
import sys
import time
import uuid
import zipfile

from client import Host, HostError
from plan import IMAGING_CONTROLS, make_plan
from trace import Trace


def paths():
    if getattr(sys, 'frozen', False):
        base = Path(sys.executable).parent
        return base, base / 'regain-device.exe', base / 'ASICamera2.dll', Path(__file__).with_name('trace-transport.js')
    portable = Path(__file__).resolve().parent.parent
    if (portable / 'regain-device.exe').is_file():
        return portable, portable / 'regain-device.exe', portable / 'ASICamera2.dll', Path(__file__).with_name('trace-transport.js')
    root = Path(__file__).resolve().parents[2]
    return root, root / 'target/debug/regain-device.exe', root / 'vendor/zwo/ASICamera2.dll', root / 'scripts/inspection/trace-transport.js'


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def select_camera(cameras, name):
    if name is None:
        if not sys.stdin.isatty():
            raise ValueError('Use --camera with an exact name from --list.')
        print('\nConnected cameras:')
        for i, c in enumerate(cameras, 1):
            print(f'  {i}. {c["name"]} ({c["width"]} x {c["height"]})')
        if not cameras:
            raise ValueError('No ASI cameras found. Install the ZWO driver and connect the camera.')
        index = int(input('Camera number: ')) - 1
        if not 0 <= index < len(cameras):
            raise ValueError('Camera number is outside the list.')
        name = cameras[index]['name']
    selected = [c for c in cameras if c['name'] == name]
    if len(selected) != 1:
        raise ValueError('Connect exactly one camera of the selected model. Duplicate names are not opened.')
    return selected[0]


def save_bundle(directory, manifest):
    # Only files created under this run are included. No log-directory scans.
    files = [p for p in directory.rglob('*') if p.is_file() and p.name not in ('manifest.json', 'SHA256SUMS')]
    manifest['files'] = [dict(path=p.relative_to(directory).as_posix(), bytes=p.stat().st_size, sha256=sha(p)) for p in sorted(files)]
    (directory / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8')
    files.append(directory / 'manifest.json')
    (directory / 'SHA256SUMS').write_text(''.join(f'{sha(p)}  {p.relative_to(directory).as_posix()}\n' for p in sorted(files)), encoding='utf-8')
    archive = directory.with_suffix('.zip')
    with zipfile.ZipFile(archive, 'x', zipfile.ZIP_DEFLATED, compresslevel=6) as output:
        for p in sorted(directory.rglob('*')):
            if p.is_file():
                output.write(p, p.relative_to(directory).as_posix())
    return archive


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--list', action='store_true', help='list SDK names without opening cameras')
    parser.add_argument('--camera', help='exact SDK camera name; otherwise show an interactive picker')
    parser.add_argument('--profile', choices=['quick', 'extended'], default='quick')
    parser.add_argument('--output', type=Path, default=Path.cwd() / 'camera-evidence')
    parser.add_argument('--include-pixels', action='store_true', help='include small-ROI raw USB chunks and corresponding SDK frames')
    parser.add_argument('--exercise-cooling', action='store_true', help='briefly change target, cooler and dew controls, then restore')
    parser.add_argument('--power-history', choices=['unknown', 'warm', 'usb-reconnected', 'cold-power-start'], default='unknown')
    parser.add_argument('--notes', default='', help='camera edition, cap/light condition, power and USB connection details')
    parser.add_argument('--deadline', type=float, default=600, help='matrix time limit in seconds (30-1800); cleanup adds at most 45 seconds')
    parser.add_argument('--plan-only', action='store_true', help='open/read capabilities and save the proposed matrix without exposures')
    parser.add_argument('--self-test', action='store_true', help='exercise the packaged tracer and simulator; no camera opened')
    args = parser.parse_args(argv)
    if not math.isfinite(args.deadline) or not 30 <= args.deadline <= 1800 or len(args.notes) > 4096:
        parser.error('deadline must be 30-1800 seconds and notes at most 4096 characters')
    base, executable, sdk, source = paths()
    for p in (executable, sdk, source):
        if not p.is_file():
            parser.error(f'Missing kit component: {p.name}. Extract the entire kit ZIP before running.')
    if args.list:
        host = Host(executable, sdk)
        try:
            cameras, _ = host.call('list')
            print(json.dumps(cameras, indent=2))
        finally:
            host.dispose()
        return 0
    if not args.camera and not args.self_test and sys.stdin.isatty():
        print('Close NINA and other camera apps. Cap the camera for dark captures.\n'
              'The kit records camera calibration and USB diagnostics locally. Nothing is uploaded.\n'
              'Imaging controls change during the exercises and are restored on normal completion.')
        args.notes = input('Camera edition, USB/power connection and cap/light condition: ')[:4096]
        args.include_pixels = input('Include small image samples for pixel-processing research? [y/N] ').strip().lower() == 'y'
        args.profile = 'extended' if input('Run extended tests (includes 60s exposure)? [y/N] ').strip().lower() == 'y' else 'quick'
    name = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ') + '-' + uuid.uuid4().hex[:8]
    directory = args.output.resolve() / name
    directory.mkdir(parents=True, exist_ok=False)
    manifest = dict(schemaVersion=1, kit='PulsarFab regain camera exercise kit', startedUtc=datetime.now(timezone.utc).isoformat(),
                    platform=platform.platform(), python=platform.python_version(), simulated=args.self_test,
                    sdkSha256=sha(sdk), hostSha256=sha(executable), tracerSha256=sha(source),
                    profile=args.profile, powerHistory=args.power_history, notes=args.notes,
                    includePixels=args.include_pixels, exerciseCooling=args.exercise_cooling,
                    completed=False, cases=[], skipped=[], restoration={}, errors=[])
    build = base / 'camera-kit-build.json'
    if build.exists():
        manifest['build'] = json.loads(build.read_text())
    host = trace = None
    original = {}
    selected = serial = None
    opened = False
    aborted = False
    run_deadline = time.monotonic() + args.deadline

    def call(method, params=None, timeout=20, sample=None):
        if trace:
            trace.checkpoint()
        return host.call(method, params, min(timeout, run_deadline - time.monotonic()), sample)[0]

    def environment():
        values = {}
        for c in (8, 15, 16, 17, 21):
            if c in caps:
                values[str(c)] = call('get-control-state', dict(control=c))
        return values

    def collect_trace():
        manifest['transport'] = dict(usbInterfaces=[dict(vid=v, pid=p) for v, p in sorted(trace.interfaces)],
                                     driverVersionBytes=sorted(trace.driver_versions), descriptors=trace.descriptors,
                                     bulkRequests=trace.bulk_count, error=trace.error, logBytes=trace.log_bytes)
        if trace.error:
            manifest['errors'].append(trace.error)
        (directory / 'sample-index.json').write_text(json.dumps(trace.samples, indent=2) + '\n', encoding='utf-8')

    try:
        host = Host(executable, sdk, args.self_test)
        trace = Trace(host, source, directory)
        host.record = trace.record
        cameras = call('list')
        selected = select_camera(cameras, 'ZWO Simulated' if args.self_test else args.camera)
        if args.self_test:
            call('simulation', dict(instant=True))
        details = call('open', dict(name=selected['name']))
        opened = True
        serial = details.get('serial')
        manifest['camera'] = details['info']
        manifest['sdkVersion'] = details['sdkVersion']
        manifest['controls'] = details['controls']
        caps = {c['type']: c for c in details['controls']}
        for c in IMAGING_CONTROLS + ((16, 17, 21) if args.exercise_cooling else ()):
            if c in caps and caps[c]['writable']:
                original[c] = call('get-control-state', dict(control=c))
        manifest['originalControls'] = original
        manifest['environmentBefore'] = environment()
        cases, manifest['skipped'] = make_plan(selected, details['controls'], args.profile)
        if args.self_test:
            cases = [c for c in cases if c['name'] in ('baseline-repeat', 'full-frame', 'bin-2', 'baseline-after-sweeps')]
        (directory / 'plan.json').write_text(json.dumps(cases, indent=2) + '\n', encoding='utf-8')
        print(f'Camera: {selected["name"]}\nProfile: {args.profile}; {len(cases)} exercises. Output: {directory}', flush=True)
        if args.plan_only:
            manifest['completed'] = True
            manifest['planOnly'] = True
        else:
            for index, case in enumerate(cases):
                trace.case = case['name']
                print(f'[{index + 1}/{len(cases)}] {case["name"]}', flush=True)
                result = dict(name=case['name'], requested=case, frames=[], outcome='failed')
                manifest['cases'].append(result)
                start = time.monotonic()
                try:
                    # Restore baseline before every variant; each case changes one variable.
                    for c, value in case['controls'].items():
                        call('set', dict(control=c, value=value))
                    result['appliedControls'] = {str(c): call('get-control-state', dict(control=c)) for c in case['controls']}
                    for frame in range(case['frames']):
                        sample_dir = None
                        if args.include_pixels and case['samples'] and frame == 0:
                            sample_dir = directory / 'samples' / case['name']
                            sample_dir.mkdir(parents=True)
                        with trace.lock:
                            trace.sample = sample_dir
                            trace.downloaded.clear()
                        call('start', case['exposure'])
                        applied_exposure = call('get-control-state', dict(control=1))
                        until = time.monotonic() + case['exposure']['microseconds'] / 1e6 + 30
                        while True:
                            status = call('status')
                            if status == 2:
                                break
                            if status != 1:
                                raise HostError(f'Exposure ended in SDK state {status}')
                            if time.monotonic() > until:
                                raise HostError('Exposure/readout deadline expired')
                            time.sleep(.025)
                        trace.checkpoint()
                        metadata, payload = host.call('download', timeout=min(30, run_deadline - time.monotonic()),
                                                      sample=sample_dir / 'sdk.raw16' if sample_dir else None)
                        if not args.self_test and not trace.downloaded.wait(5):
                            raise HostError('SDK download completed without its trace event')
                        trace.checkpoint()
                        usb_sequences = [s['sequence'] for s in trace.samples if sample_dir and s['exercise'] == case['name']]
                        if sample_dir and not args.self_test and not usb_sequences:
                            raise HostError('SDK sample has no captured USB chunks; pixel evidence is incomplete')
                        if (payload['bytes'] != case['exposure']['width'] * case['exposure']['height'] * 2
                            or metadata.get('width') != case['exposure']['width'] or metadata.get('height') != case['exposure']['height']):
                            raise HostError('Incomplete RAW16 frame')
                        result['frames'].append(dict(**payload, metadata=metadata, format='RAW16 little-endian',
                                                     appliedExposure=applied_exposure,
                                                     usbSequences=usb_sequences,
                                                     sample=(sample_dir / 'sdk.raw16').relative_to(directory).as_posix() if sample_dir else None))
                        with trace.lock:
                            trace.sample = None
                    result['environmentAfter'] = environment()
                    result['outcome'] = 'passed'
                except Exception as error:
                    result['error'] = str(error)
                    print(f'  Recorded failure: {error}', flush=True)
                    if host.broken or host.proc.poll() is not None or trace.error:
                        raise
                    call('stop', timeout=5)
                finally:
                    with trace.lock:
                        trace.sample = None
                    result['elapsedMs'] = round((time.monotonic() - start) * 1000)
                if time.monotonic() >= run_deadline:
                    raise HostError('Matrix deadline expired')
            if args.exercise_cooling:
                trace.case = 'environment-controls'
                measured = environment()
                manifest['coolingExercise'] = dict(before=measured)
                for c in (16, 17, 21):
                    if c not in original:
                        continue
                    if c in (16, 17) and ('8' not in measured or 16 not in original):
                        manifest['coolingExercise'].setdefault('skipped', []).append(c)
                        continue
                    value = max(caps[c]['min'], min(caps[c]['max'], round(measured['8']['value'] / 10) - 2)) if c == 16 else 1
                    manifest['coolingExercise'].setdefault('applied', {})[str(c)] = call('set-control-state', dict(control=c, value=value, auto=False))
                time.sleep(min(5, max(0, run_deadline - time.monotonic())))
                manifest['coolingExercise']['after'] = environment()
            trace.case = 'reopen'
            call('close')
            opened = False
            again = call('open', dict(name=selected['name'], serial=serial))
            opened = True
            if again.get('serial') != serial:
                raise HostError('Camera identity changed on reopen')
            manifest['reopen'] = 'same identity, same SDK process; no power cycle'
            manifest['completed'] = True
        manifest['environmentBeforeRestore'] = environment()
    except KeyboardInterrupt:
        aborted = True
        manifest['errors'].append('Canceled by user; remaining exercises were not run')
    except Exception as error:
        manifest['errors'].append(str(error).replace(str(base), '[kit]').replace(str(directory), '[run]'))
        print(f'Capture stopped: {manifest["errors"][-1]}', file=sys.stderr)
    finally:
        # Bounded best-effort restore, including automatic flags. A dead host can
        # be reopened only with the established serial; never switch cameras.
        restore_until = time.monotonic() + 45
        try:
            if original and host and (host.broken or host.proc.poll() is not None) and serial:
                if trace:
                    collect_trace()
                    trace.close()
                    trace = None
                host.dispose()
                host = Host(executable, sdk, args.self_test)
                host.call('open', dict(name=selected['name'], serial=serial), timeout=15)
                opened = True
            if opened and host and host.proc.poll() is None:
                if trace:
                    trace.case = 'restore'
                try:
                    host.call('stop', timeout=min(5, restore_until - time.monotonic()))
                except Exception as error:
                    manifest['errors'].append(f'Stop during cleanup failed: {error}')
                for c, saved in sorted(original.items(), key=lambda item: (item[0] == 17, item[0])):
                    try:
                        actual, _ = host.call('set-control-state', dict(control=c, **saved), timeout=min(5, restore_until - time.monotonic()))
                        manifest['restoration'][str(c)] = dict(expected=saved, actual=actual, matched=actual == saved)
                    except Exception as error:
                        manifest['restoration'][str(c)] = dict(expected=saved, matched=False, error=str(error))
                manifest['environmentAfterRestore'] = {
                    str(c): host.call('get-control-state', dict(control=c), timeout=min(5, restore_until - time.monotonic()))[0]
                    for c in (8, 15, 16, 17, 21) if c in caps
                }
                host.call('close', timeout=min(5, restore_until - time.monotonic()))
            elif original:
                manifest['errors'].append('Unable to reopen original camera for control restoration')
        except Exception as error:
            manifest['errors'].append(f'Control restoration incomplete: {error}')
        finally:
            if trace:
                collect_trace()
                trace.close()
            if host:
                host.dispose()
        if not args.self_test and not args.plan_only and not manifest.get('transport', {}).get('bulkRequests'):
            manifest['errors'].append('No supported bulk transport observed; SDK success alone is not a usable protocol trace')
        if any(not item['matched'] for item in manifest['restoration'].values()):
            manifest['errors'].append('Some controls did not restore exactly; inspect restoration values')
        manifest['finishedUtc'] = datetime.now(timezone.utc).isoformat()
        manifest['passed'] = manifest['completed'] and not manifest['errors'] and all(c['outcome'] == 'passed' for c in manifest['cases'])
        (directory / 'READ-ME.txt').write_text(
            'Local camera research evidence. Nothing has been uploaded.\n'
            'Review manifest.json, plan.json and events.jsonl before sharing this ZIP.\n'
            'Control payloads can contain camera identifiers and factory calibration.\n'
            'Optional samples contain actual image pixels, including any scene in front of the camera.\n'
            'USB chunks are individual completed reads, not reconstructed or validated frames.\n'
            'RAW16 SDK samples are little-endian; dimensions and hashes are in manifest.json.\n'
            'A failed case is useful evidence, but does not prove that camera mode is supported.\n'
            'ROI/format and SDK dark-subtraction state are not restored; configure your next imaging session normally.\n', encoding='utf-8')
        archive = save_bundle(directory, manifest)
        print(f'\nBundle: {archive}\nSHA256: {sha(archive)}\n'
              f'Passed: {sum(c["outcome"] == "passed" for c in manifest["cases"])}/{len(manifest["cases"])}; '
              f'completed: {manifest["completed"]}; review errors and restoration in manifest.json.', flush=True)
    return 130 if aborted else 0 if manifest['passed'] else 2


if __name__ == '__main__':
    try:
        code = main()
    except (Exception, KeyboardInterrupt) as error:
        print(f'Camera kit stopped: {error}', file=sys.stderr)
        code = 130 if isinstance(error, KeyboardInterrupt) else 2
    if len(sys.argv) == 1 and sys.stdin.isatty():
        input('Press Enter to close. ')
    sys.exit(code)
