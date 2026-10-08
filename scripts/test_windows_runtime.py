"""PE dependency gate regressions; no library or hardware is activated."""
import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest


spec = importlib.util.spec_from_file_location("windows_runtime", Path(__file__).with_name("check-windows-runtime.py"))
runtime = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runtime)


def fixture(*, bits=64, ordinary="KERNEL32.dll", delayed=None, legacy_delay=False):
    data = bytearray(0x800)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3c, 0x80)
    data[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<H", data, 0x86, 1)
    optional_size = 240 if bits == 64 else 224
    struct.pack_into("<H", data, 0x94, optional_size)
    optional = 0x98
    struct.pack_into("<H", data, optional, 0x20b if bits == 64 else 0x10b)
    struct.pack_into("<Q" if bits == 64 else "<I", data, optional + (24 if bits == 64 else 28), 0x400000)
    struct.pack_into("<I", data, optional + 60, 0x200)
    struct.pack_into("<I", data, optional + (108 if bits == 64 else 92), 16)
    directory = optional + (112 if bits == 64 else 96)
    struct.pack_into("<IIII", data, optional + optional_size + 8, 0x600, 0x1000, 0x600, 0x200)
    if ordinary:
        struct.pack_into("<II", data, directory + 8, 0x1000, 40)
        struct.pack_into("<IIIII", data, 0x200, 0, 0, 0, 0x1100, 0)
        text = ordinary.encode("ascii") + b"\0"
        data[0x300:0x300 + len(text)] = text
    if delayed:
        struct.pack_into("<II", data, directory + 13 * 8, 0x1040, 64)
        struct.pack_into("<II", data, 0x240, 0 if legacy_delay else 1, 0x401140 if legacy_delay else 0x1140)
        text = delayed.encode("ascii") + b"\0"
        data[0x340:0x340 + len(text)] = text
    return data


class WindowsRuntimeTests(unittest.TestCase):
    def test_ordinary_and_delayed_imports_in_both_architectures(self):
        for bits in (32, 64):
            with self.subTest(bits=bits), tempfile.TemporaryDirectory() as temporary:
                binary = Path(temporary) / "worker.exe"
                binary.write_bytes(fixture(bits=bits, delayed="USER32.dll"))
                self.assertEqual(runtime.imports(binary), ["KERNEL32.dll", "USER32.dll"])
                self.assertIn(str(binary), runtime.audit([temporary]))

    def test_versioned_runtime_imports_are_rejected_even_in_nested_vendor_dlls(self):
        for dll in ("VCRUNTIME140.dll", "vcruntime140_1.dll", "MSVCP140.dll", "msvcr120.dll", "concrt140.dll", "vcomp140.dll"):
            for delayed in (False, True):
                with self.subTest(dll=dll, delayed=delayed), tempfile.TemporaryDirectory() as temporary:
                    binary = Path(temporary) / "nested/vendor.dll"
                    binary.parent.mkdir()
                    binary.write_bytes(fixture(ordinary=None if delayed else dll, delayed=dll if delayed else None))
                    with self.assertRaisesRegex(ValueError, "requires VC\\+\\+ runtime DLLs"):
                        runtime.audit([temporary])

    def test_legacy_delay_import_virtual_addresses_are_checked(self):
        with tempfile.TemporaryDirectory() as temporary:
            binary = Path(temporary) / "worker.exe"
            binary.write_bytes(fixture(bits=32, delayed="vcruntime140.dll", legacy_delay=True))
            with self.assertRaisesRegex(ValueError, "requires VC\\+\\+ runtime DLLs"):
                runtime.audit([binary])

    def test_os_crt_names_are_not_mistaken_for_redistributables(self):
        with tempfile.TemporaryDirectory() as temporary:
            for dll in ("msvcrt.dll", "ucrtbase.dll", "api-ms-win-crt-runtime-l1-1-0.dll", "msvcp_win.dll"):
                binary = Path(temporary) / dll
                binary.write_bytes(fixture(ordinary=dll))
            self.assertEqual(len(runtime.audit([temporary])), 4)

    def test_malformed_or_truncated_imports_fail_closed(self):
        invalid = [b"not PE", fixture()[:0x500]]
        bad_address = fixture()
        struct.pack_into("<I", bad_address, 0x20c, 0x9000)
        invalid.append(bad_address)
        unterminated = fixture()
        struct.pack_into("<I", unterminated, 0x98 + 112 + 12, 20)
        invalid.append(unterminated)
        for data in invalid:
            with self.subTest(length=len(data)), tempfile.TemporaryDirectory() as temporary:
                binary = Path(temporary) / "bad.dll"
                binary.write_bytes(data)
                with self.assertRaises(ValueError):
                    runtime.audit([temporary])

    def test_missing_or_empty_payloads_fail_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            for path in (Path(temporary), Path(temporary) / "missing"):
                with self.assertRaises(ValueError):
                    runtime.audit([path])

    def test_real_bundled_sdk_has_no_vc_runtime_dependency(self):
        vendor = Path(__file__).resolve().parents[1] / "vendor/zwo/ASICamera2.dll"
        self.assertIn("KERNEL32.dll", runtime.audit([vendor])[str(vendor)])


if __name__ == "__main__":
    unittest.main()
