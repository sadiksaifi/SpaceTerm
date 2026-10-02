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
SPEC = importlib.util.spec_from_file_location("run_platform_task", SCRIPTS / "run-platform-task.py")
DISPATCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DISPATCH)
PLATFORM_SEGMENT = {"darwin": "macos", "linux": "linux"}


class DispatchTests(unittest.TestCase):
    def test_each_supported_platform_dispatches_every_profile_to_its_own_task(self):
        tasks = tomllib.loads((ROOT / ".mise.toml").read_text())["tasks"]
        for platform, segment in PLATFORM_SEGMENT.items():
            for profile in ("development", "development:workbench"):
                task = DISPATCH.platform_task(profile, platform)
                self.assertTrue(task in tasks, f"{task} is not a mise task")
                self.assertIn(segment, task.split(":"))

    def test_unsupported_platform_has_no_task(self):
        self.assertIsNone(DISPATCH.platform_task("development", "win32"))


FAKE_ARTIFACTS = """#!/bin/sh
[ "$1" = run ] && [ "$2" = -- ] || exit 97
shift 2
exec "$@"
"""

FAKE_BUILD = """import pathlib, sys
arguments = sys.argv[1:]
output = pathlib.Path(arguments[arguments.index("--output") + 1])
assert "--locked" in arguments
assert "--features" not in arguments
executable = pathlib.Path(__file__).resolve().parent.parent / "target" / "debug" / "spaceterm"
executable.parent.mkdir(parents=True, exist_ok=True)
executable.write_text(
    "#!/bin/sh\\n"
    f"printf '%s\\\\n' \\"$0\\" \\"${{SPACETERM_DEVELOPER_WORKBENCH:-}}\\" > \\"$SPACETERM_TEST_RECORD\\"\\n"
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
        (scripts / "build-cargo-executable.py").write_text(FAKE_BUILD)
        shutil.copytree(ROOT / "assets" / "shell-integration", self.root / "assets" / "shell-integration")
        shutil.copytree(ROOT / "assets" / "terminfo", self.root / "assets" / "terminfo")
        self.record = self.root / "record"

    def run_profile(self, *arguments, section=""):
        environment = {**os.environ, "SPACETERM_TEST_RECORD": str(self.record), "TMPDIR": str(self.root)}
        environment["SPACETERM_DEVELOPER_WORKBENCH"] = section
        return subprocess.run(
            ["bash", str(self.root / "scripts" / "run-development-app-linux.sh"), *arguments],
            env=environment,
            capture_output=True,
            text=True,
        )

    def test_dev_profile_launches_from_a_private_prefix_with_installed_resources(self):
        result = self.run_profile()
        self.assertEqual(result.returncode, 0, result.stderr)
        prefix = self.root / "target" / "development-apps" / "development"
        self.assertEqual(
            self.record.read_text().splitlines(),
            [str(prefix / "bin" / "spaceterm"), ""],
        )
        self.assertTrue((prefix / "share" / "spaceterm" / "shell-integration" / "zsh").is_dir())
        self.assertTrue((prefix / "share" / "spaceterm" / "terminfo" / "x" / "xterm-spaceterm").is_file())
        self.assertEqual(
            [path.name for path in prefix.parent.iterdir()],
            ["development"],
            "the staging directory must be renamed into place",
        )

    def test_development_launch_preserves_the_workbench_request(self):
        result = self.run_profile(section="controls")
        self.assertEqual(result.returncode, 0, result.stderr)
        prefix = self.root / "target" / "development-apps" / "development"
        self.assertEqual(
            self.record.read_text().splitlines(),
            [str(prefix / "bin" / "spaceterm"), "controls"],
        )

    def test_unknown_profile_is_rejected_before_building(self):
        result = self.run_profile("release")
        self.assertEqual(result.returncode, 2)
        self.assertFalse((self.root / "target").exists())


if __name__ == "__main__":
    unittest.main()
