#!/usr/bin/env python3
"""Exercise local GPUI patch ownership in an isolated Git repository."""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("gpui-local.py")
FORK_URL = "https://github.com/sadiksaifi/zed"
LOCK = f'''version = 4

[[package]]
name = "gpui"
version = "0.2.0"
source = "git+{FORK_URL}?tag=test#1234"

[[package]]
name = "unrelated"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
'''


class GpuiLocalTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "scripts").mkdir()
        (self.root / "scripts" / "gpui-local.py").write_bytes(SCRIPT.read_bytes())
        (self.root / "Cargo.lock").write_text(LOCK)
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
        subprocess.run(["git", "add", "Cargo.lock"], cwd=self.root, check=True)
        subprocess.run(
            ["git", "-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-qm", "fixture"],
            cwd=self.root,
            check=True,
        )
        for name in ("first", "second"):
            checkout = self.root / name
            (checkout / "crates" / "gpui").mkdir(parents=True)
            (checkout / "SPACETERM.md").touch()
            (checkout / "crates" / "gpui" / "Cargo.toml").write_text(
                '[package]\nname = "gpui"\nversion = "0.2.0"\n'
            )
        self.config = self.root / ".cargo" / "config.toml"

    def run_script(self, mode, checkout="first"):
        subprocess.run(
            [sys.executable, str(self.root / "scripts" / "gpui-local.py"), mode, checkout],
            cwd=self.root,
            check=True,
        )

    def test_repeated_on_keeps_patch_after_lockfile_sources_change(self):
        self.run_script("on")
        first = self.config.read_text()
        (self.root / "Cargo.lock").write_text(LOCK.replace(f'git+{FORK_URL}?tag=test#1234', ''))
        self.run_script("on")
        self.assertIn('gpui = { path = ', first)
        self.assertEqual(self.config.read_text(), first)

    def test_on_switches_checkout_after_lockfile_sources_change(self):
        self.run_script("on")
        (self.root / "Cargo.lock").write_text(LOCK.replace(f'git+{FORK_URL}?tag=test#1234', ''))
        self.run_script("on", "second")
        self.assertIn("/second/crates/gpui", self.config.read_text())
        self.assertNotIn("/first/crates/gpui", self.config.read_text())

    def test_on_and_off_preserve_unrelated_configuration(self):
        self.config.parent.mkdir()
        original = '[build]\nrustflags = ["-C", "debuginfo=1"]\n'
        self.config.write_text(original)
        self.run_script("on")
        self.assertIn(original, self.config.read_text())
        self.run_script("off")
        self.assertEqual(self.config.read_text(), original)

    def test_off_without_on_preserves_configuration(self):
        self.config.parent.mkdir()
        original = '[build]\ntarget-dir = "target/custom"\n'
        self.config.write_text(original)
        self.run_script("off")
        self.assertEqual(self.config.read_text(), original)

    def test_off_restores_configuration_without_final_newline(self):
        self.config.parent.mkdir()
        original = '[build]\ntarget-dir = "target/custom"'
        self.config.write_text(original)
        self.run_script("on")
        self.run_script("off")
        self.assertEqual(self.config.read_text(), original)


if __name__ == "__main__":
    unittest.main()
