"""License collection must preserve notices and fail on unverified omissions."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import rust_licenses


class RustLicenseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "crate"
        self.source.mkdir()
        self.package = dict(id="used", name="example", version="1.0.0", source="registry",
                            manifest_path=str(self.source / "Cargo.toml"), license_file=None)
        self.metadata = dict(workspace_members=["workspace"], packages=[self.package], resolve=dict(nodes=[
            dict(id="workspace", deps=[dict(pkg="used")]), dict(id="used", deps=[])]))
        self.destination = self.root / "licenses"

    def test_only_resolved_registry_packages_and_all_notices_are_staged(self):
        (self.source / "LICENSE-MIT").write_text("license", encoding="utf-8")
        (self.source / "NOTICE").write_text("copyright notice", encoding="utf-8")
        (self.source / "README.md").write_text("not a license", encoding="utf-8")
        self.metadata["packages"].extend([dict(self.package, id="unresolved", name="unused", manifest_path=str(self.root / "missing/Cargo.toml")),
                                           dict(self.package, id="workspace", name="workspace", source=None)])
        records = rust_licenses.stage(self.metadata, self.destination)
        self.assertEqual(len(records), 1)
        self.assertEqual(records[0]["origin"], "crate")
        self.assertEqual([file["name"] for file in records[0]["files"]], ["LICENSE-MIT", "NOTICE"])
        self.assertEqual((self.destination / "example-1.0.0/NOTICE").read_text(), "copyright notice")
        self.assertEqual(json.loads((self.destination / "rust-license-manifest.json").read_text()), records)

    def test_declared_nested_license_is_included_and_cannot_escape_the_crate(self):
        (self.source / "legal").mkdir()
        (self.source / "LICENSE").write_text("root license", encoding="utf-8")
        (self.source / "legal/LICENSE").write_text("declared license", encoding="utf-8")
        self.package["license_file"] = "legal/LICENSE"
        rust_licenses.stage(self.metadata, self.destination)
        self.assertEqual((self.destination / "example-1.0.0/legal/LICENSE").read_text(), "declared license")
        self.assertEqual((self.destination / "example-1.0.0/LICENSE").read_text(), "root license")
        self.package["license_file"] = "../LICENSE"
        with self.assertRaisesRegex(RuntimeError, "escaped declared license"):
            rust_licenses.package_texts(self.package)

    def test_unknown_missing_license_fails_before_any_copy(self):
        known = self.root / "known"
        known.mkdir()
        (known / "LICENSE").write_text("known text", encoding="utf-8")
        self.metadata["packages"].insert(0, dict(self.package, id="known", name="known", manifest_path=str(known / "Cargo.toml")))
        self.metadata["resolve"]["nodes"][0]["deps"].append(dict(pkg="known"))
        self.metadata["resolve"]["nodes"].append(dict(id="known", deps=[]))
        with self.assertRaisesRegex(RuntimeError, "Missing license text"):
            rust_licenses.stage(self.metadata, self.destination)
        self.assertFalse(self.destination.exists())

    def test_delivered_license_bytes_must_match_the_preflight_hash(self):
        (self.source / "LICENSE").write_text("original", encoding="utf-8")
        copy_file = rust_licenses.shutil.copy2

        def corrupt(source, destination):
            copy_file(source, destination)
            Path(destination).write_text("changed after preflight", encoding="utf-8")

        with patch.object(rust_licenses.shutil, "copy2", side_effect=corrupt):
            with self.assertRaisesRegex(RuntimeError, "changed during staging"):
                rust_licenses.stage(self.metadata, self.destination)
        self.assertFalse((self.destination / "rust-license-manifest.json").exists())

    def repair(self):
        self.package.update(name="asn1-rs-impl", version="0.2.0", license="MIT/Apache-2.0",
                            repository="https://github.com/rusticata/asn1-rs.git",
                            source="registry+https://github.com/rust-lang/crates.io-index")
        overrides = self.root / "repairs"
        folder = overrides / "asn1-rs-impl-0.2.0"
        folder.mkdir(parents=True)
        record = json.loads((rust_licenses.OVERRIDES / "asn1-rs-impl-0.2.0/PROVENANCE.json").read_text())
        original = b"fixture manifest\n"
        (self.source / "Cargo.toml.orig").write_bytes(original)
        (self.source / ".cargo_vcs_info.json").write_text(json.dumps(record["vcs"]), encoding="utf-8")
        record["manifestSha256"] = hashlib.sha256(original).hexdigest()
        for name in record["files"]:
            data = (rust_licenses.OVERRIDES / "asn1-rs-impl-0.2.0" / name).read_bytes()
            self.assertEqual(hashlib.sha256(data).hexdigest(), record["files"][name]["sha256"])
            (folder / name).write_bytes(data)
        (folder / "PROVENANCE.json").write_text(json.dumps(record), encoding="utf-8")
        return overrides, folder

    def test_exact_pinned_upstream_texts_and_provenance_are_staged(self):
        overrides, _ = self.repair()
        records = rust_licenses.stage(self.metadata, self.destination, overrides)
        self.assertEqual(records[0]["origin"], "pinned-upstream")
        self.assertEqual({file["name"] for file in records[0]["files"]}, {"LICENSE-APACHE", "LICENSE-MIT", "PROVENANCE.json"})
        text = (self.destination / "asn1-rs-impl-0.2.0/LICENSE-MIT").read_text()
        self.assertIn("Copyright (c) 2017 Pierre Chifflier", text)

    def test_changed_source_and_package_identity_cannot_borrow_the_repair(self):
        overrides, _ = self.repair()
        for key in ("license", "repository", "source"):
            with self.subTest(key=key):
                package = dict(self.package)
                package[key] = "changed"
                with self.assertRaisesRegex(RuntimeError, "package identity changed"):
                    rust_licenses.package_texts(package, overrides)
        for key in ("name", "version"):
            with self.subTest(key=key):
                package = dict(self.package)
                package[key] = "changed"
                with self.assertRaisesRegex(RuntimeError, "Missing license text"):
                    rust_licenses.package_texts(package, overrides)
        vcs_path = self.source / ".cargo_vcs_info.json"
        original = vcs_path.read_bytes()
        vcs = json.loads(original)
        vcs["git"]["sha1"] = "0" * 40
        vcs_path.write_text(json.dumps(vcs), encoding="utf-8")
        with self.assertRaisesRegex(RuntimeError, "source changed"):
            rust_licenses.package_texts(self.package, overrides)
        vcs_path.write_bytes(original)
        (self.source / "Cargo.toml.orig").write_text("changed", encoding="utf-8")
        with self.assertRaisesRegex(RuntimeError, "source changed"):
            rust_licenses.package_texts(self.package, overrides)

    def test_changed_license_bytes_are_rejected(self):
        overrides, folder = self.repair()
        with (folder / "LICENSE-MIT").open("a", encoding="utf-8") as stream:
            stream.write("changed")
        with self.assertRaisesRegex(RuntimeError, "Pinned license text changed"):
            rust_licenses.package_texts(self.package, overrides)


if __name__ == "__main__":
    unittest.main()
