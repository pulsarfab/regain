"""Static research tests: never load the SDK or enumerate/open cameras."""
from contextlib import redirect_stdout
import io
from pathlib import Path
import tempfile
import unittest

from inspect_usb2_sdk import DEFAULT, inspect


class Usb2SdkInspectionTests(unittest.TestCase):
    def test_digest_rejects_unreviewed_binary_before_parsing(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "not-an-sdk"
            path.write_bytes(b"not an ELF")
            with self.assertRaisesRegex(ValueError, "digest differs"):
                inspect(path, ".*")

    def test_pinned_sdk_links_usb_speed_to_the_bandwidth_branch(self):
        output = io.StringIO()
        with redirect_stdout(output):
            inspect(DEFAULT, r"CCameraBase10OpenCamera|CCameraFX3.*(IsUSB3Host|SetFPGABandWidth)|CCameraS2600MM_Duo10SetFPSPerc", True)
        result = output.getvalue()
        for evidence in [
            "_ZN10CCameraFX310IsUSB3HostEv@plt",
            "mov byte ptr [rbx + 0x12c], al",
            "mov esi, 0xb4",
            "imul ecx, ecx, 0xa908",
            "f32=400000",
            "f32=25600",
            "f32=256]",
            "_ZN10CCameraFX316SetFPGABandWidthEf@plt",
        ]:
            self.assertIn(evidence, result)

    def test_unknown_symbol_does_not_silently_succeed(self):
        with self.assertRaisesRegex(ValueError, "No matching"):
            inspect(DEFAULT, "DefinitelyNotAnSdkSymbol")

    def test_address_disambiguates_local_working_functions(self):
        output = io.StringIO()
        with redirect_stdout(output):
            inspect(DEFAULT, "WorkingFunc", False, 0x13b730)
        self.assertIn("1 matching symbols", output.getvalue())
        self.assertIn("0x13b730", output.getvalue())
        with self.assertRaisesRegex(ValueError, "No matching"):
            inspect(DEFAULT, "WorkingFunc", False, 1)


if __name__ == "__main__":
    unittest.main()
