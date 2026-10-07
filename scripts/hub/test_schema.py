"""Verify the Rust-generated structural contract with an independent validator."""
import copy
import json
from pathlib import Path
import unittest

from jsonschema import Draft202012Validator, FormatChecker

ROOT = Path(__file__).resolve().parents[2]
DESCRIPTION = json.loads((ROOT / "contracts/hub-config.json").read_text(encoding="utf-8"))
SCHEMA = DESCRIPTION["schema"]
VALIDATOR = Draft202012Validator(SCHEMA, format_checker=FormatChecker())


def example(name):
    return json.loads((ROOT / f"crates/regain-hub/examples/{name}.json").read_text(encoding="utf-8"))


class SchemaContractTests(unittest.TestCase):
    def test_native_camera_recovery_uses_legacy_limits_and_class_constraints(self):
        config = example("two-source-safety")
        backend = {"kind": "native", "device": "camera-direct", "identity": "PRIVATE-CAMERA",
                   "camera": {"model": "ZWO ASI585MM Pro", "sdkFallback": True,
                              "recovery": {"maxRetries": 0, "directReadRetries": 5,
                                           "reconnectDelaySeconds": 1e-6, "usbPortCycle": True}}}
        config["sources"][0]["backend"] = backend
        VALIDATOR.validate(config)
        for patch in [{"maxRetires": 0}, {"maxRetries": 21}, {"coolingStableSamples": 0},
                      {"reconnectDelaySeconds": 0}, {"commandTimeoutSeconds": 3601},
                      {"maximumRetryExposureSeconds": 86401}, {"directReadRetries": 6},
                      {"usbPortCycle": 1}, {"readyFrameDownloadRetries": -1}]:
            changed = copy.deepcopy(config)
            changed["sources"][0]["backend"]["camera"]["recovery"].update(patch)
            with self.subTest(patch=patch):
                self.assertTrue(list(VALIDATOR.iter_errors(changed)))
        for device in ["camera-sdk", "efw", "ofp2"]:
            changed = copy.deepcopy(config)
            changed["sources"][0]["backend"]["device"] = device
            self.assertTrue(list(VALIDATOR.iter_errors(changed)))
        backend["device"] = "camera-sdk"
        backend["camera"]["sdkFallback"] = False
        VALIDATOR.validate(config)
        for missing in [None, "absent"]:
            changed = copy.deepcopy(config)
            if missing is None:
                changed["sources"][0]["backend"]["camera"] = None
            else:
                changed["sources"][0]["backend"].pop("camera")
            self.assertTrue(list(VALIDATOR.iter_errors(changed)))

    def test_schema_and_examples(self):
        Draft202012Validator.check_schema(SCHEMA)
        for name in ["two-source-safety", "mixed-switch"]:
            VALIDATOR.validate(example(name))

    def test_missing_required_fields_unknown_fields_types_and_bounds(self):
        baseline = example("two-source-safety")
        changes = [
            lambda c: c.pop("schemaVersion"),
            lambda c: c.update(schemaVersion=2),
            lambda c: c.update(revision="not-a-uuid"),
            lambda c: c["sources"][0]["backend"].update(password="inline-secret"),
            lambda c: c["sources"][0]["backend"].pop("deviceType"),
            lambda c: c["sources"][0].update(polling={"attemptsPerCycle": 0}),
            lambda c: c["sources"][0].update(polling={"attemptsPerCycle": 1.5}),
            lambda c: c["outputs"][0]["device"]["members"][0].update(policy={"maximumSafeAgeSeconds": -1}),
        ]
        for change in changes:
            with self.subTest(change=changes.index(change)):
                changed = copy.deepcopy(baseline)
                change(changed)
                self.assertTrue(list(VALIDATOR.iter_errors(changed)))

    def test_tagged_backend_fields_are_conditional(self):
        config = example("two-source-safety")
        config["sources"][0]["backend"] = {
            "kind": "com", "progId": "ASCOM.Example.SafetyMonitor",
            "deviceType": "safetymonitor", "bitness": "x64"
        }
        VALIDATOR.validate(config)
        config["sources"][0]["backend"]["baseUrl"] = "http://localhost/"
        self.assertTrue(list(VALIDATOR.iter_errors(config)))

    def test_ui_keeps_semantic_validation_in_the_engine(self):
        # A valid structure can still have unsafe semantics. The Rust engine's
        # config tests reject this timing; frontends must call validateConfig.
        config = example("two-source-safety")
        config["outputs"][0]["device"]["members"][0]["policy"] = {"maximumSafeAgeSeconds": 1}
        VALIDATOR.validate(config)

    def test_native_filter_metadata_uses_shared_arrays_reference_and_int32_bounds(self):
        config = example("two-source-safety")
        backend = {"kind": "native", "device": "efw", "identity": "PRIVATE-WHEEL"}
        config["sources"][0]["backend"] = backend
        VALIDATOR.validate(config)
        backend["filterWheel"] = {"names": ["L", "Hα", ""], "focusOffsets": [0, -12, 17]}
        VALIDATOR.validate(config)
        for changes in [
            {"names": []}, {"names": [1]}, {"names": ["L"] * 1025},
            {"focusOffsets": []}, {"focusOffsets": [1, 2]},
            {"focusOffsets": [0, 1.5]}, {"focusOffsets": [0, "1"]},
            {"focusOffsets": [0, 2147483648]}, {"focusOffsets": [-2147483649, 0]},
        ]:
            changed = copy.deepcopy(config)
            changed["sources"][0]["backend"]["filterWheel"].update(changes)
            with self.subTest(changes=changes):
                self.assertTrue(list(VALIDATOR.iter_errors(changed)))
        backend["filterWheel"]["focusOffsets"] = [0]
        # Cross-array slot counts and native class still belong to semantic
        # review; structural acceptance cannot authorize Apply.
        VALIDATOR.validate(config)


    def test_panel_simulation_update_contract(self):
        validator = Draft202012Validator(DESCRIPTION["simulationControl"]["schema"])
        for update in [{"brightness": 0}, {"brightness": 2147483647, "maxBrightness": 2147483647, "calibratorState": 3},
                       {"coverState": 4, "coverMoving": False}, {"lightDurationSeconds": 300},
                       {"calibratorState": 0, "calibratorChanging": False}]:
            validator.validate({"coverCalibrator": update})
        for update in [{"brightness": -1}, {"brightness": 2147483648}, {"brightness": 1.5}, {"brightness": "0"},
                       {"maxBrightness": 0}, {"coverState": 6}, {"calibratorState": -1}, {"coverMoving": 0},
                       {"lightDurationSeconds": 301}, {"moveDurationSeconds": -1}, {"extra": True}]:
            with self.subTest(update=update):
                self.assertTrue(list(validator.iter_errors({"coverCalibrator": update})))

    def test_wheel_simulation_update_contract(self):
        schema = DESCRIPTION["simulationControl"]["schema"]
        Draft202012Validator.check_schema(schema)
        validator = Draft202012Validator(schema)
        for patch in [{"filterWheel": {"position": -1}},
                      {"filterWheel": {"names": ["L", "Hα", ""], "focusOffsets": [-2147483648, 0, 2147483647]}},
                      {"filterWheel": {"moveDurationSeconds": 300}, "fault": "stalledMotion"}]:
            validator.validate(patch)
        for update in [{"names": []}, {"names": [0]}, {"names": [""] * 1025},
                       {"focusOffsets": []}, {"focusOffsets": [1]}, {"focusOffsets": [0, 2147483648]},
                       {"focusOffsets": [-2147483649, 0]}, {"focusOffsets": [0, 1.5]},
                       {"position": -2}, {"position": 1024}, {"position": "0"},
                       {"moveDurationSeconds": -1}, {"moveDurationSeconds": 301}, {"halt": True}]:
            with self.subTest(update=update):
                self.assertTrue(list(validator.iter_errors({"filterWheel": update})))


if __name__ == "__main__":
    unittest.main()
