#!/usr/bin/env python3
"""Exercise local AccessKit patch ownership in isolated checkouts."""

from pathlib import Path
import subprocess
import shutil
import sys
import tempfile
import tomllib
import unittest


SCRIPT = Path(__file__).with_name("accesskit-local.py")
CRATES = {"accesskit": "common", "accesskit_consumer": "consumer", "accesskit_atspi_common": "platforms/atspi-common"}


class AccessKitLocalTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        (self.root / "scripts").mkdir()
        (self.root / "scripts/accesskit-local.py").write_bytes(SCRIPT.read_bytes())
        self.config = self.root / ".cargo/config.toml"
        self.config.parent.mkdir()
        for name in ("first", "second"):
            checkout = self.root / name
            checkout.mkdir()
            (checkout / "SPACETERM.md").touch()
            for crate, directory in CRATES.items():
                manifest = checkout / directory / "Cargo.toml"
                manifest.parent.mkdir(parents=True)
                manifest.write_text(f'[package]\nname = "{crate}"\nversion = "0.1.0"\n')

    def run_script(self, mode, checkout="first", success=True):
        result = subprocess.run(
            [sys.executable, str(self.root / "scripts/accesskit-local.py"), mode, checkout],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode == 0, success, result.stderr)

    def test_non_bmp_checkout_paths_produce_valid_toml(self):
        checkout = self.root / 'accesskit-😀 "quote" \\slash'
        shutil.copytree(self.root / "first", checkout)
        self.run_script("on", str(checkout))
        patches = tomllib.loads(self.config.read_text())["patch"]["crates-io"]
        for name, directory in CRATES.items():
            self.assertEqual(patches[name]["path"], str(checkout / directory))
        self.run_script("off")
        self.assertFalse(self.config.exists())

    def test_switching_checkout_replaces_all_three_patches(self):
        self.run_script("on")
        self.run_script("on", "second")
        patches = tomllib.loads(self.config.read_text())["patch"]["crates-io"]
        self.assertEqual(set(patches), set(CRATES))
        for name, directory in CRATES.items():
            self.assertEqual(patches[name]["path"], str(self.root / "second" / directory))

    def test_on_and_off_preserve_environment_and_gpui_configuration(self):
        original = '[env]\nMACOSX_DEPLOYMENT_TARGET = "26.0"\n[patch."https://github.com/sadiksaifi/zed"]\ngpui = { path = "../zed/crates/gpui" }'
        self.config.write_text(original)
        self.run_script("on")
        first = self.config.read_text()
        self.run_script("on")
        self.assertEqual(self.config.read_text(), first)
        self.run_script("off")
        self.assertEqual(self.config.read_text(), original)

    def test_off_removes_only_generated_configuration(self):
        self.run_script("on")
        self.run_script("off")
        self.assertFalse(self.config.exists())
        self.run_script("off")

    def test_invalid_checkout_does_not_replace_active_override(self):
        self.run_script("on")
        original = self.config.read_text()
        (self.root / "second/consumer/Cargo.toml").unlink()
        self.run_script("on", "second", success=False)
        self.assertEqual(self.config.read_text(), original)

    def test_unowned_patch_table_and_incomplete_markers_are_preserved(self):
        for original in ('[patch.crates-io]\nother = { path = "../other" }\n', '# BEGIN SpaceTerm local AccessKit patches\n'):
            with self.subTest(original=original):
                self.config.write_text(original)
                self.run_script("on", success=False)
                self.assertEqual(self.config.read_text(), original)


if __name__ == "__main__":
    unittest.main()
