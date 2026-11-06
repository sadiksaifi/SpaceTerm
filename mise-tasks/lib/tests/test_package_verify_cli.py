"""Preflight verification CLIs report success without treating evidence as an exit code."""

import errno
import subprocess
import sys
import unittest

from spaceterm_tasks import ROOT


class PackageVerifyCliTests(unittest.TestCase):
    def invoke(self, platform, outcome, *arguments):
        return subprocess.run(
            [
                sys.executable,
                "-c",
                f"""
import runpy
import sys
from unittest.mock import patch
from spaceterm_tasks import TaskError, package_{platform}
def verify(*args, **kwargs):
    {outcome}
sys.argv = [{str(ROOT / f"mise-tasks/preflight/verify/{platform}.py")!r}, *sys.argv[1:]]
with patch.object(package_{platform}, "verify", side_effect=verify):
    runpy.run_path(sys.argv[0], run_name="__main__")
""",
                *arguments,
            ],
            capture_output=True,
            text=True,
            timeout=10,
        )

    def test_successful_verification_exits_zero_with_evidence(self):
        for platform in ("linux", "macos"):
            with self.subTest(platform=platform):
                result = self.invoke(platform, "return {'build': {}, 'core_sha256': {}}")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stderr, "")

    def test_verification_failures_and_argument_errors_exit_nonzero(self):
        for platform in ("linux", "macos"):
            for outcome, arguments, status, message in (
                ("raise TaskError('verification refused')", (), 1, "verification refused"),
                (f"raise OSError({errno.EACCES}, 'private native error')", (), 1, "EACCES"),
                ("raise RuntimeError('unexpected failure')", (), 1, "unexpected failure"),
                ("raise KeyboardInterrupt", (), 130, ""),
                ("raise AssertionError('must not verify')", ("--invalid",), 2, "unrecognized"),
            ):
                with self.subTest(platform=platform, outcome=outcome, arguments=arguments):
                    result = self.invoke(platform, outcome, *arguments)
                    self.assertEqual(result.returncode, status, result.stderr)
                    self.assertIn(message, result.stderr)
                    self.assertNotIn("private native error", result.stderr)
