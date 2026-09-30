#!/usr/bin/env python3
"""Test development-app dispatch and the Linux private-prefix staging script."""

import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
ROOT = SCRIPTS.parent
SPEC = importlib.util.spec_from_file_location("run_development_app", SCRIPTS / "run-development-app.py")
DISPATCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DISPATCH)
PLATFORM_SEGMENT = {"darwin": "macos", "linux": "linux"}


class DispatchTests(unittest.TestCase):
    def test_each_supported_platform_dispatches_every_profile_to_its_own_task(self):
        tasks = tomllib.loads((ROOT / ".mise.toml").read_text())["tasks"]
        for platform, segment in PLATFORM_SEGMENT.items():
            for profile in DISPATCH.PROFILES:
                task = DISPATCH.task_for_platform(profile, platform)
                self.assertTrue(task in tasks, f"{task} is not a mise task")
                self.assertIn(segment, task.split(":"))

    def test_unsupported_platform_has_no_task(self):
        self.assertIsNone(DISPATCH.task_for_platform("dev", "win32"))

    def test_unknown_profile_is_rejected(self):
        with self.assertRaises(ValueError):
            DISPATCH.task_for_platform("release", "linux")


FAKE_ARTIFACTS = """#!/bin/sh
[ "$1" = run ] && [ "$2" = -- ] || exit 97
shift 2
exec "$@"
"""

FAKE_BUILD = """import pathlib, sys
arguments = sys.argv[1:]
output = pathlib.Path(arguments[arguments.index("--output") + 1])
features = arguments[arguments.index("--features") + 1]
executable = pathlib.Path(__file__).resolve().parent.parent / "target" / "debug" / "spaceterm"
executable.parent.mkdir(parents=True, exist_ok=True)
executable.write_text(
    "#!/bin/sh\\n"
    f"printf '%s\\\\n' \\"$0\\" \\"${{SPACETERM_APPEARANCE_EXERCISER:-}}\\" {features} > \\"$SPACETERM_TEST_RECORD\\"\\n"
)
executable.chmod(0o755)
output.write_text(str(executable))
"""


@unittest.skipUnless(
    sys.platform == "linux" and shutil.which("tic"), "the Linux prefix is staged with GNU tools and tic"
)
class LinuxPrefixTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        scripts = self.root / "scripts"
        scripts.mkdir()
        shutil.copy2(SCRIPTS / "run-development-app-linux.sh", scripts)
        (scripts / "cargo-artifacts.sh").write_text(FAKE_ARTIFACTS)
        (scripts / "cargo-artifacts.sh").chmod(0o755)
        (scripts / "cargo-build-executable.py").write_text(FAKE_BUILD)
        shutil.copytree(ROOT / "assets" / "shell-integration", self.root / "assets" / "shell-integration")
        shutil.copytree(ROOT / "assets" / "terminfo", self.root / "assets" / "terminfo")
        self.record = self.root / "record"

    def run_profile(self, profile):
        environment = {**os.environ, "SPACETERM_TEST_RECORD": str(self.record), "TMPDIR": str(self.root)}
        environment.pop("SPACETERM_APPEARANCE_EXERCISER", None)
        return subprocess.run(
            ["bash", str(self.root / "scripts" / "run-development-app-linux.sh"), profile],
            env=environment,
            capture_output=True,
            text=True,
        )

    def test_dev_profile_launches_from_a_private_prefix_with_installed_resources(self):
        result = self.run_profile("dev")
        self.assertEqual(result.returncode, 0, result.stderr)
        prefix = self.root / "target" / "development-apps" / "dev"
        self.assertEqual(
            self.record.read_text().splitlines(),
            [str(prefix / "bin" / "spaceterm"), "", "development-app"],
        )
        self.assertTrue((prefix / "share" / "spaceterm" / "shell-integration" / "zsh").is_dir())
        self.assertTrue((prefix / "share" / "spaceterm" / "terminfo" / "x" / "xterm-spaceterm").is_file())
        self.assertEqual(
            [path.name for path in prefix.parent.iterdir()],
            ["dev"],
            "the staging directory must be renamed into place",
        )

    def test_appearance_profile_enables_the_exerciser(self):
        result = self.run_profile("appearance")
        self.assertEqual(result.returncode, 0, result.stderr)
        prefix = self.root / "target" / "development-apps" / "appearance"
        self.assertEqual(
            self.record.read_text().splitlines(),
            [str(prefix / "bin" / "spaceterm"), "1", "appearance-exerciser"],
        )

    def test_unknown_profile_is_rejected_before_building(self):
        result = self.run_profile("release")
        self.assertEqual(result.returncode, 2)
        self.assertFalse((self.root / "target").exists())


if __name__ == "__main__":
    unittest.main()
