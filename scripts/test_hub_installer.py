"""File-only installer fixture checks. Never register COM or connect equipment."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import uuid

from jsonschema import Draft202012Validator, FormatChecker

SCRIPT = Path(__file__).with_name("test-hub-installer.py")
if sys.platform == "win32":
    spec = importlib.util.spec_from_file_location("hub_installer_fixture", SCRIPT)
    fixture = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(fixture)


@unittest.skipUnless(sys.platform == "win32", "Windows installer fixture")
class HubInstallerTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="regain-installer-files-")
        self.addCleanup(self.directory.cleanup)
        self.folder = Path(self.directory.name)

    def prepare(self, name):
        return fixture.prepare_state(self.folder / f"{name} install", self.folder / name, "S-1-5-21-123-456-789-1001")

    def test_all_classes_have_schema_valid_metadata_only_configuration(self):
        state = self.prepare("Primary")
        config = json.loads((self.folder / "Primary/hub.json").read_text())
        schema = json.loads((fixture.ROOT / "contracts/hub-config.json").read_text(encoding="utf-8"))["schema"]
        Draft202012Validator(schema, format_checker=FormatChecker()).validate(config)
        kinds = [entry["binding"]["deviceType"] for entry in state["entries"]]
        self.assertEqual(kinds, ["switch", "safetymonitor", "observingconditions", "focuser", "rotator", "filterwheel", "covercalibrator", "camera"])
        self.assertEqual(len(config["sources"]), 8)
        self.assertEqual(config["outputs"][0]["device"]["channels"][2]["units"], "°C")
        sources = {source["id"]: source for source in config["sources"]}
        for output, entry in zip(config["outputs"], state["entries"], strict=True):
            self.assertEqual(entry["binding"]["instanceId"], config["instanceId"])
            self.assertEqual(entry["binding"]["outputId"], output["id"])
            self.assertTrue(entry["binding"]["simulated"])
            if output["device"]["kind"] == "proxy":
                backend = sources[output["device"]["source"]]["backend"]
                self.assertEqual(backend["deviceType"], entry["binding"]["deviceType"])
                if backend["kind"] == "alpaca":
                    self.assertEqual(backend["baseUrl"], "http://127.0.0.1:1")
        saved = json.loads(Path(state["bindings"]).read_text())
        self.assertEqual(saved["revision"], state["revision"])
        self.assertEqual(saved["bindings"], [entry["binding"] for entry in state["entries"]])
        identities = json.loads((self.folder / "Primary/identities.json").read_text())
        self.assertEqual([identity["clsid"] for identity in identities], [entry["clsid"] for entry in state["entries"]])
        self.assertEqual([identity["version"] for identity in identities], [3, 3, 2, 4, 4, 3, 2, 4])
        fixture.assert_settings(state)

    def test_known_identities_and_chooser_classes(self):
        expected = [
            ("switch", "S", "Switch", "69a5917f-8d71-5a9d-b3e7-8d5a53f88e0b"),
            ("safetymonitor", "M", "SafetyMonitor", "14421c22-3804-5450-91c1-211547b06944"),
            ("observingconditions", "W", "ObservingConditions", "f3d3f0d7-9c8d-5b4d-834c-04b0805a56ff"),
            ("focuser", "F", "Focuser", "e8862a89-95df-5b67-8555-142cfcdc810f"),
            ("rotator", "R", "Rotator", "7c9d3910-2aa2-5ef1-addd-f2db0c7dd14f"),
            ("filterwheel", "L", "FilterWheel", "1419052d-9e99-5c56-b922-ea0157112e83"),
            ("covercalibrator", "C", "CoverCalibrator", "f09687bc-5ccd-5e1d-ac89-5ef12b5a8ed3"),
            ("camera", "A", "Camera", "a2575d79-310e-5296-8c5e-4a48acdbf6dc"),
        ]
        for kind, prefix, chooser, clsid in expected:
            with self.subTest(kind=kind):
                binding = dict(instanceId="10000000-0000-0000-0000-000000000001", outputId="20000000-0000-0000-0000-000000000002", deviceType=kind)
                paths = fixture.paths(binding)
                progid = "Rgn.H" + prefix + "." + uuid.UUID(clsid).hex
                self.assertEqual(paths[0], "Software\\Classes\\CLSID\\{" + clsid + "}")
                self.assertEqual(paths[1], "Software\\Classes\\" + progid)
                self.assertEqual(paths[3], f"Software\\ASCOM\\{chooser} Drivers\\{progid}")
                self.assertEqual(len(progid), 39)

    def test_two_installs_have_disjoint_identities_and_settings(self):
        first, second = self.prepare("Primary"), self.prepare("Other")
        self.assertTrue({entry["clsid"] for entry in first["entries"]}.isdisjoint(entry["clsid"] for entry in second["entries"]))
        self.assertTrue(set(first["settings"]).isdisjoint(second["settings"]))
        Path(first["bindings"]).unlink()
        fixture.assert_settings(first, bindings_present=False)
        fixture.assert_settings(second)
        with self.assertRaises(FileNotFoundError):
            fixture.assert_settings(first)

    def test_settings_mutation_and_recreated_bindings_are_detected(self):
        state = self.prepare("Primary")
        path = Path(state["bindings"])
        original = path.read_bytes()
        path.write_bytes(original + b" ")
        with self.assertRaisesRegex(AssertionError, "Setup changed Hub settings"):
            fixture.assert_settings(state)
        path.unlink()
        fixture.assert_settings(state, bindings_present=False)
        path.write_bytes(original)
        with self.assertRaisesRegex(AssertionError, "Deleted fixture bindings reappeared"):
            fixture.assert_settings(state, bindings_present=False)

    def test_local_and_optimized_execution_cannot_register(self):
        environment = dict(os.environ)
        environment.pop("GITHUB_ACTIONS", None)
        environment.pop("RUNNER_OS", None)
        for flags, message in (([], "elevated disposable GitHub Windows runner"), (["-O"], "requires Python assertions")):
            result = subprocess.run([sys.executable, *flags, str(SCRIPT), "prepare", "--install", str(self.folder / "install"), "--fixture", str(self.folder / "must not exist")],
                                    env=environment, capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(message, result.stderr)
            self.assertFalse((self.folder / "must not exist").exists())


if __name__ == "__main__":
    unittest.main()
