#!/usr/bin/env python3
"""Exercise the commit-message hook through real Git commits."""

import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


class CommitHookTests(unittest.TestCase):
    def test_hook_accepts_conventional_commits_and_rejects_other_subjects(self):
        with tempfile.TemporaryDirectory() as directory:
            def git(*args):
                return subprocess.run(["git", "-C", directory, *args], capture_output=True, text=True)

            for args in [("init", "-q"), ("config", "user.name", "Test"),
                         ("config", "user.email", "test@example.invalid"),
                         ("config", "core.hooksPath", str(ROOT / ".githooks"))]:
                self.assertEqual(git(*args).returncode, 0)
            for message in ("feat: add panes", "fix(updates)!: preserve sessions\n\nBREAKING CHANGE: require macOS 26.", "docs: explain setup"):
                result = git("commit", "--allow-empty", "-m", message)
                self.assertEqual(result.returncode, 0, result.stderr)
            before = git("rev-parse", "HEAD").stdout
            for message in ("update stuff", "fix:", "fix:   ", "Fix: title", "fix(): title", "fix: title\nbody without separator"):
                result = git("commit", "--allow-empty", "-m", message)
                self.assertNotEqual(result.returncode, 0, message)
                self.assertIn("Conventional Commit", result.stderr)
                self.assertEqual(git("rev-parse", "HEAD").stdout, before)


if __name__ == "__main__":
    unittest.main()
