import json
import io
import struct
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch
import zipfile

from camera_kit import save_bundle, select_camera, sha
from client import Host, HostError
from plan import IMAGING_CONTROLS, MAX_FRAME, make_plan
from trace import Trace
from build import package

CAMERA = dict(name='ZWO test', width=6248, height=4176, bins=[1, 2, 3, 4], formats=[0, 2])
CONTROLS = [dict(type=c, min=lo, max=hi, value=v, writable=True) for c, lo, hi, v in
            [(0, -25, 700, 100), (1, 32, 2000000000, 100000), (5, 0, 240, 50), (6, 40, 100, 40), (9, 0, 3, 0)]]


class PlanTests(unittest.TestCase):
    def test_variants_are_bounded_and_do_not_change_environment(self):
        for profile in ('quick', 'extended'):
            cases, skipped = make_plan(CAMERA, CONTROLS, profile)
            self.assertFalse(skipped)
            baseline = cases[0]
            self.assertEqual(baseline['frames'], 2)
            for case in cases:
                e = case['exposure']
                self.assertLessEqual(e['width'] * e['height'] * 2, MAX_FRAME)
                self.assertEqual(e['width'] % 8, 0)
                self.assertEqual(e['height'] % 2, 0)
                self.assertLessEqual((e['x'] + e['width']) * e['bin'], CAMERA['width'])
                self.assertLessEqual((e['y'] + e['height']) * e['bin'], CAMERA['height'])
                self.assertTrue(set(case['controls']) <= set(IMAGING_CONTROLS))
                changed = [k for k, v in case['controls'].items() if v != baseline['controls'][k]]
                self.assertLessEqual(len(changed), 1)
                if changed:
                    self.assertEqual(e, baseline['exposure'])
            if profile == 'extended':
                self.assertIn(60000000, [c['exposure']['microseconds'] for c in cases])

    def test_unsupported_ranges_and_huge_frames_are_recorded_as_skips(self):
        controls = [dict(c, max=1000000) if c['type'] == 1 else c for c in CONTROLS]
        cases, skipped = make_plan(dict(CAMERA, width=20000, height=20000), controls, 'extended')
        self.assertTrue(any(c['name'] == 'full-frame' for c in skipped))
        self.assertTrue(any(c['name'] == 'exposure-60000000us' for c in skipped))
        self.assertTrue(all(c['exposure']['microseconds'] <= 1000000 for c in cases))

    def test_same_name_devices_are_never_opened_by_index(self):
        self.assertEqual(select_camera([CAMERA], CAMERA['name']), CAMERA)
        with self.assertRaises(ValueError):
            select_camera([CAMERA, CAMERA], CAMERA['name'])
        with self.assertRaises(ValueError):
            select_camera([CAMERA], 'another camera')

    def test_unknown_pixel_format_is_rejected(self):
        with self.assertRaises(ValueError):
            make_plan(dict(CAMERA, formats=[0]), CONTROLS, 'quick')

    def test_bundle_hashes_and_no_overwrite(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp) / 'run'
            directory.mkdir()
            (directory / 'events.jsonl').write_text('{"kind":"test"}\n')
            manifest = dict(completed=False, errors=['test failure'])
            archive = save_bundle(directory, manifest)
            with zipfile.ZipFile(archive) as zipped:
                saved = json.loads(zipped.read('manifest.json'))
                self.assertFalse(saved['completed'])
                self.assertEqual(saved['files'][0]['sha256'], sha(directory / 'events.jsonl'))
                self.assertEqual(set(zipped.namelist()), {'manifest.json', 'events.jsonl', 'SHA256SUMS'})
            with self.assertRaises(FileExistsError):
                save_bundle(directory, manifest)


class ProtocolTests(unittest.TestCase):
    def test_mismatched_or_oversized_replies_kill_the_owned_host(self):
        for reply in (dict(version=1, id=2, binaryLength=0, ok=True),
                      dict(version=1, id=1, binaryLength=2**40, ok=True)):
            encoded = json.dumps(reply).encode()
            proc = Mock(stdin=io.BytesIO(), stdout=io.BytesIO(struct.pack('<I', len(encoded)) + encoded))
            proc.poll.return_value = None
            with patch('client.subprocess.Popen', return_value=proc):
                host = Host('unused', 'unused')
                try:
                    with self.assertRaises(HostError):
                        host.call('list')
                    proc.kill.assert_called_once()
                finally:
                    host.dispose()

    def test_truncated_frame_is_never_accepted(self):
        encoded = json.dumps(dict(version=1, id=1, binaryLength=8, ok=True, result={})).encode()
        proc = Mock(stdin=io.BytesIO(), stdout=io.BytesIO(struct.pack('<I', len(encoded)) + encoded + b'abc'))
        with patch('client.subprocess.Popen', return_value=proc):
            host = Host('unused', 'unused')
            try:
                with self.assertRaises(HostError):
                    host.call('download')
            finally:
                host.dispose()


class PackagingTests(unittest.TestCase):
    def test_package_hashes_the_finished_signed_payload(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            kit = root / 'kit'
            kit.mkdir()
            (root / 'artifacts').mkdir()
            (kit / 'camera-kit-build.json').write_text(json.dumps(dict(version='0.1.0.0')))
            for name in ('Regain-CameraKit.exe', 'regain-host.exe', 'ASICamera2.dll', 'README.md'):
                (kit / name).write_bytes(b'prepared payload')
            package(kit, root, '0.1.0.0')
            (kit / 'Regain-CameraKit.exe').write_bytes(b'payload with appended signature')
            package(kit, root, '0.1.0.0')
            archive = root / 'artifacts/Regain-CameraKit-0.1.0.0-win-x64.zip'
            self.assertEqual(archive.with_suffix('.zip.sha256').read_text().split()[0], sha(archive))
            with zipfile.ZipFile(archive) as zipped:
                self.assertEqual(zipped.read('Regain-CameraKit/Regain-CameraKit.exe'), b'payload with appended signature')
                sums = zipped.read('Regain-CameraKit/SHA256SUMS').decode()
                self.assertIn(sha(kit / 'Regain-CameraKit.exe') + '  Regain-CameraKit.exe', sums)
            with self.assertRaises(ValueError):
                package(kit, root, '0.2.0.0')


class TraceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.directory = Path(self.temp.name)
        source = self.directory / 'hooks.js'
        source.write_text('// unit test')
        session = Mock()
        script = session.create_script.return_value
        callbacks = {}
        script.on.side_effect = lambda name, callback: callbacks.update({name: callback})
        script.load.side_effect = lambda: callbacks['message']({'type': 'send', 'payload': {'kind': 'trace-ready'}}, None)
        with patch('trace.frida.attach', return_value=session):
            self.trace = Trace(Mock(), source, self.directory)

    def tearDown(self):
        self.trace.close()
        self.temp.cleanup()

    def test_paths_omitted_and_descriptors_associated_with_their_device(self):
        self.trace.message(dict(type='send', payload=dict(kind='device-open', handle='0x10',
                           path=r'\\?\usb#vid_03c3&pid_2601#private-instance')), None)
        self.trace.message(dict(type='send', payload=dict(kind='io-submit', sequence=1, handle='0x10',
                           code='0x220020', header='8006000100001200')), None)
        self.trace.message(dict(type='send', payload=dict(kind='control-payload', sequence=1, data='1201')), None)
        log = (self.directory / 'events.jsonl').read_text()
        self.assertNotIn('private-instance', log)
        self.assertEqual(self.trace.descriptors[0]['usb'], dict(vid='03c3', pid='2601'))

    def test_pixels_are_not_saved_without_opt_in(self):
        self.trace.message(dict(type='send', payload=dict(kind='bulk-hash-input', sequence=1)), b'pixel data')
        self.assertFalse(self.trace.samples)
        self.assertFalse(list(self.directory.rglob('*.bin')))
        self.assertIn('sha256', (self.directory / 'events.jsonl').read_text())

    def test_log_and_pixel_limits_stop_collection(self):
        with patch('trace.MAX_LOG', 1):
            self.trace.record(dict(kind='test'))
            self.assertIn('incomplete', self.trace.error)
        self.trace.error = None
        self.trace.sample = self.directory
        with patch('trace.MAX_SAMPLES', 1):
            self.trace.message(dict(type='send', payload=dict(kind='bulk-hash-input', sequence=1)), b'pixels')
            self.assertIn('incomplete', self.trace.error)
            self.assertFalse(list(self.directory.glob('*.bin')))


if __name__ == '__main__':
    unittest.main()
