"""Exercise read-only ASCOM registration catalogs, never activate drivers.

Valid handshakes read the installed registry view. Private registry fixtures
separately prove deterministic contents and elevation precedence in both bitnesses.
"""
import argparse
import json
from pathlib import Path
import subprocess
import unittest
import uuid

WORKERS = Path(__file__).resolve().parents[1] / "target/debug"


class LocalCatalog(unittest.TestCase):
    def run_catalog(self, architecture, device_type, barrier, extra=()):
        command = [str(WORKERS / "hub-ascom" / architecture / "Regain.Hub.ASCOM.exe"),
                   "--discover", "--device-type", device_type, "--bitness", architecture, *extra]
        return subprocess.run(command, input=barrier, capture_output=True, timeout=25,
                              creationflags=subprocess.CREATE_NO_WINDOW)

    def test_all_classes_in_both_actual_worker_architectures(self):
        for architecture in ("x86", "x64"):
            for device_type in ("camera", "switch", "safetymonitor", "observingconditions",
                                "focuser", "rotator", "filterwheel", "covercalibrator"):
                with self.subTest(architecture=architecture, device_type=device_type):
                    result = self.run_catalog(architecture, device_type, b'{"command":"discover"}\n')
                    self.assertEqual(result.returncode, 0)
                    self.assertEqual(result.stderr, b"")
                    self.assertLessEqual(len(result.stdout), 1024 * 1024)
                    catalog = json.loads(result.stdout)
                    self.assertEqual(set(catalog), {"entries", "incomplete"})
                    self.assertIsInstance(catalog["incomplete"], bool)
                    self.assertLessEqual(len(catalog["entries"]), 256)
                    identities = set()
                    for entry in catalog["entries"]:
                        self.assertEqual(set(entry), {"name", "progId", "classId"})
                        self.assertTrue(entry["name"].strip())
                        self.assertLessEqual(len(entry["name"]), 200)
                        self.assertRegex(entry["progId"], r"\A[A-Za-z0-9._-]{1,200}\Z")
                        self.assertNotIn(entry["progId"].lower(), identities)
                        identities.add(entry["progId"].lower())
                        if entry["classId"] is not None:
                            self.assertNotEqual(uuid.UUID(entry["classId"]).int, 0)

    def test_rejected_barriers_and_arguments_produce_no_catalog(self):
        for architecture in ("x86", "x64"):
            for barrier in (b"", b"bad\n", b'{"command":"discover","extra":true}\n',
                            b'{"command":"discover"}', b"x" * 4097 + b"\n"):
                with self.subTest(architecture=architecture, barrier_length=len(barrier)):
                    result = self.run_catalog(architecture, "focuser", barrier)
                    self.assertEqual(result.returncode, 2)
                    self.assertEqual(result.stdout, b"")
            for device_type, extra in (("telescope", ()), ("focuser", ("--simulate",))):
                result = self.run_catalog(architecture, device_type, b"", extra)
                self.assertEqual(result.returncode, 2)
                self.assertEqual(result.stdout, b"")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--workers", type=Path, default=WORKERS)
    args, remaining = parser.parse_known_args()
    WORKERS = args.workers.resolve()
    unittest.main(argv=[__file__, *remaining])
