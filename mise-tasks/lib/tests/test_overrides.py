"""Exercise local GPUI and AccessKit patch ownership in isolated Git repositories."""

import shutil
import subprocess
import tempfile
import tomllib
import unittest
from pathlib import Path

from spaceterm_tasks import TaskError
from spaceterm_tasks.overrides import ACCESSKIT, ACCESSKIT_CRATES, GPUI, ZED_FORK, disable, enable

LOCK = f"""version = 4

[[package]]
name = "gpui"
version = "0.2.0"
source = "git+{ZED_FORK}?tag=test#1234"

[[package]]
name = "unrelated"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"""


class OverrideTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        (self.root / "Cargo.lock").write_text(LOCK)
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
        subprocess.run(["git", "add", "Cargo.lock"], cwd=self.root, check=True)
        subprocess.run(
            [
                "git",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ],
            cwd=self.root,
            check=True,
        )
        for name in ("zed", "zed-second"):
            checkout = self.root / name
            (checkout / "crates" / "gpui").mkdir(parents=True)
            (checkout / "SPACETERM.md").touch()
            (checkout / "crates" / "gpui" / "Cargo.toml").write_text(
                '[package]\nname = "gpui"\nversion = "0.2.0"\n'
            )
        for name in ("accesskit", "accesskit-second"):
            checkout = self.root / name
            checkout.mkdir()
            (checkout / "SPACETERM.md").touch()
            for crate, directory in ACCESSKIT_CRATES.items():
                manifest = checkout / directory / "Cargo.toml"
                manifest.parent.mkdir(parents=True)
                manifest.write_text(f'[package]\nname = "{crate}"\nversion = "0.1.0"\n')
        self.config = self.root / ".cargo" / "config.toml"
        self.config.parent.mkdir()

    def enable(self, override, checkout):
        enable(override, checkout, root=self.root)

    def disable(self, override):
        disable(override, root=self.root)

    def contents(self):
        return self.config.read_text() if self.config.exists() else ""

    def test_interleaved_overrides_restore_configuration_in_either_order(self):
        checkouts = {GPUI: "zed", ACCESSKIT: "accesskit"}
        for original in ('[env]\nX = "1"', '[env]\nX = "1"\n', ""):
            for first, second in ((GPUI, ACCESSKIT), (ACCESSKIT, GPUI)):
                for removal in ((first, second), (second, first)):
                    with self.subTest(
                        original=original,
                        first=first.name,
                        removal=[override.name for override in removal],
                    ):
                        self.config.write_text(original)
                        self.enable(first, checkouts[first])
                        self.enable(second, checkouts[second])
                        self.disable(removal[0])
                        patches = tomllib.loads(self.config.read_text())["patch"]
                        self.assertEqual(set(patches), {removal[1].source})
                        self.enable(removal[1], checkouts[removal[1]])
                        self.disable(removal[1])
                        self.assertEqual(self.contents(), original)

    def test_repeated_gpui_override_keeps_its_patch_after_lockfile_sources_change(self):
        self.enable(GPUI, "zed")
        first = self.config.read_text()
        (self.root / "Cargo.lock").write_text(LOCK.replace(f"git+{ZED_FORK}?tag=test#1234", ""))
        self.enable(GPUI, "zed")
        self.assertIn("gpui = { path = ", first)
        self.assertEqual(self.config.read_text(), first)

    def test_switching_checkout_replaces_every_patch(self):
        for override, first, second, crates in (
            (GPUI, "zed", "zed-second", {"gpui": "crates/gpui"}),
            (ACCESSKIT, "accesskit", "accesskit-second", ACCESSKIT_CRATES),
        ):
            with self.subTest(override=override.name):
                self.enable(override, first)
                self.enable(override, second)
                patches = tomllib.loads(self.config.read_text())["patch"][override.source]
                self.assertEqual(set(patches), set(crates))
                for name, directory in crates.items():
                    self.assertEqual(patches[name]["path"], str(self.root / second / directory))
                self.disable(override)

    def test_non_bmp_checkout_paths_produce_valid_toml(self):
        checkout = self.root / 'accesskit-😀 "quote" \\slash'
        shutil.copytree(self.root / "accesskit", checkout)
        self.enable(ACCESSKIT, str(checkout))
        patches = tomllib.loads(self.config.read_text())["patch"]["crates-io"]
        for name, directory in ACCESSKIT_CRATES.items():
            self.assertEqual(patches[name]["path"], str(checkout / directory))
        self.disable(ACCESSKIT)
        self.assertFalse(self.config.exists())

    def test_overrides_preserve_unrelated_configuration(self):
        for original in (
            '[build]\nrustflags = ["-C", "debuginfo=1"]\n',
            '[build]\ntarget-dir = "target/custom"',
            f'[env]\nMACOSX_DEPLOYMENT_TARGET = "26.0"\n[patch."{ZED_FORK}"]\n'
            'gpui = { path = "../zed/crates/gpui" }',
        ):
            with self.subTest(original=original):
                self.config.write_text(original)
                self.disable(ACCESSKIT)
                self.assertEqual(self.config.read_text(), original)
                self.enable(ACCESSKIT, "accesskit")
                first = self.config.read_text()
                self.assertIn(original, first)
                self.enable(ACCESSKIT, "accesskit")
                self.assertEqual(self.config.read_text(), first)
                self.disable(ACCESSKIT)
                self.assertEqual(self.config.read_text(), original)

    def test_disabling_the_only_override_removes_the_generated_file(self):
        self.enable(GPUI, "zed")
        self.disable(GPUI)
        self.assertFalse(self.config.exists())
        self.disable(GPUI)

    def test_invalid_checkout_does_not_replace_the_active_override(self):
        self.enable(ACCESSKIT, "accesskit")
        original = self.config.read_text()
        (self.root / "accesskit-second/consumer/Cargo.toml").unlink()
        (self.root / "zed-second/SPACETERM.md").unlink()
        with self.assertRaises(TaskError):
            self.enable(ACCESSKIT, "accesskit-second")
        with self.assertRaises(TaskError):
            self.enable(GPUI, "zed-second")
        self.assertEqual(self.config.read_text(), original)

    def test_unowned_patch_tables_and_incomplete_markers_are_preserved(self):
        for override, original in (
            (ACCESSKIT, '[patch.crates-io]\nother = { path = "../other" }\n'),
            (ACCESSKIT, "# BEGIN SpaceTerm local AccessKit patches\n"),
            (GPUI, f'[patch."{ZED_FORK}"]\ngpui = {{ path = "../other" }}\n'),
            (GPUI, "# END SpaceTerm local GPUI patches\n"),
        ):
            with self.subTest(override=override.name, original=original):
                self.config.write_text(original)
                with self.assertRaises(TaskError):
                    self.enable(override, "zed" if override is GPUI else "accesskit")
                self.assertEqual(self.config.read_text(), original)


if __name__ == "__main__":
    unittest.main()
