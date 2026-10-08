"""Acceptance-harness cleanup regressions; no application or hardware I/O."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("omni_acceptance", Path(__file__).with_name("test-hub-omnisimulator.py"))
omni = importlib.util.module_from_spec(spec)
spec.loader.exec_module(omni)


class CleanupTests(unittest.TestCase):
    def test_pending_open_and_one_poll_gap_do_not_claim_disconnection(self):
        quiet = omni.QuietDisconnect()
        for state in [(True, False), (False, False), (False, True),
                      (False, False), (False, False), (True, False),
                      (False, False), (False, False)]:
            self.assertFalse(quiet(state), state)
        self.assertTrue(quiet((False, False)))

    def test_numeric_or_missing_flags_cannot_prove_cleanup(self):
        for state in [(0, 0), (), (False,), (False, None)]:
            quiet = omni.QuietDisconnect()
            for _ in range(4):
                self.assertFalse(quiet(state))
        with self.assertRaises(RuntimeError):
            omni.connection_state(lambda kind, member: 0, "camera", 4)

    def test_legacy_camera_does_not_request_unsupported_connecting(self):
        members = []
        def read(kind, member):
            self.assertEqual(kind, "camera")
            members.append(member)
            return False
        self.assertEqual(omni.connection_state(read, "camera", 3), (False, False))
        self.assertEqual(members, ["connected"])
        members.clear()
        self.assertEqual(omni.connection_state(read, "camera", 4), (False, False))
        self.assertEqual(members, ["connecting", "connected"])


if __name__ == "__main__":
    unittest.main()
