"""Catalog updates must not invent releases for source-only provider packages."""
import json
import os
import pathlib
import runpy
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = pathlib.Path(__file__).with_name("sync-release-catalog.py")


class SyncReleaseCatalogTests(unittest.TestCase):
    def test_source_only_entry_keeps_local_install_and_has_no_release_url(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "catalog").mkdir()
            catalog = root / "catalog/packages.json"
            catalog.write_text(json.dumps([
                {"name": "provider", "install": "rpi package link deepseek --path packages/provider", "prebuilt_manifest": "stale"},
                {"name": "published", "install": "rpi install published --path packages/published --github old/upstream", "nameZh": "发布包", "nameEn": "Published package", "descriptionZh": "保留目录中的中文描述。", "descriptionEn": "Keep this catalog description.", "description": "Keep this catalog description."},
                {"name": "prebuilt-only", "install": "rpi install prebuilt-only --path packages/prebuilt-only --github old/upstream"},
            ]), encoding="utf-8")
            metadata = {"packages": [
                {"name": "provider", "version": "0.1.0", "publish": [], "repository": "https://github.com/example/packages"},
                {"name": "published", "version": "1.2.3", "publish": None, "metadata": None, "repository": "https://github.com/example/packages"},
                {"name": "prebuilt-only", "version": "0.1.0", "publish": [], "repository": "https://github.com/example/packages", "metadata": {"rpi": {"prebuilt": True}}},
            ]}
            previous = pathlib.Path.cwd()
            try:
                os.chdir(root)
                with patch("subprocess.check_output", return_value=json.dumps(metadata).encode()):
                    runpy.run_path(str(SCRIPT), run_name="__main__")
                    first_sync = catalog.read_text(encoding="utf-8")
                    runpy.run_path(str(SCRIPT), run_name="__main__")
                    self.assertEqual(catalog.read_text(encoding="utf-8"), first_sync)
            finally:
                os.chdir(previous)
            source, published, prebuilt = json.loads(catalog.read_text(encoding="utf-8"))
            self.assertEqual(source["version"], "0.1.0")
            self.assertEqual(source["install"], "rpi package link deepseek --path packages/provider")
            self.assertNotIn("prebuilt_manifest", source)
            self.assertEqual(published["description"], "Keep this catalog description.")
            self.assertEqual(published["nameZh"], "发布包")
            self.assertEqual(published["nameEn"], "Published package")
            self.assertEqual(published["descriptionZh"], "保留目录中的中文描述。")
            self.assertEqual(published["descriptionEn"], published["description"])
            self.assertEqual(published["prebuilt_manifest"], "https://github.com/example/packages/releases/download/published-v1.2.3/rpi-extension.json")
            self.assertEqual(prebuilt["prebuilt_manifest"], "https://github.com/example/packages/releases/download/prebuilt-only-v0.1.0/rpi-extension.json")
            self.assertEqual(published["install"], "rpi install published --github example/packages")
            self.assertEqual(prebuilt["install"], "rpi install prebuilt-only --github example/packages")

    def test_all_catalog_cards_have_bilingual_names_and_descriptions(self):
        path = SCRIPT.parents[2] / "catalog/packages.json"
        entries = json.loads(path.read_text(encoding="utf-8"))
        for entry in entries:
            for field in ["nameZh", "nameEn", "descriptionZh", "descriptionEn"]:
                with self.subTest(package=entry["name"], field=field):
                    self.assertIsInstance(entry.get(field), str)
                    self.assertTrue(entry[field].strip())
            with self.subTest(package=entry["name"]):
                self.assertRegex(entry["descriptionZh"], r"[\u3400-\u9fff]")
                self.assertNotEqual(entry["descriptionZh"], entry["descriptionEn"])
                self.assertEqual(entry["description"], entry["descriptionEn"])

    def test_catalog_install_sources_do_not_mix_and_card_descriptions_exist(self):
        path = SCRIPT.parents[2] / "catalog/packages.json"
        entries = json.loads(path.read_text(encoding="utf-8"))
        for entry in entries:
            install = entry.get("install", "")
            self.assertFalse("--path" in install and "--github" in install, entry["name"])
        by_name = {entry["name"]: entry for entry in entries}
        self.assertEqual(by_name["rpi-btw"]["install"], "rpi install rpi-btw --github pi-rust/rpi-extension")
        for name in ["rpi-codemode", "rpi-edit-diagnostics"]:
            self.assertTrue(by_name[name].get("description", "").strip(), name)


if __name__ == "__main__":
    unittest.main()
