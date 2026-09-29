#!/usr/bin/env python3
"""Test the Cargo artifact supervisor's control of its guarded process group."""

import importlib.util
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "cargo_artifact_supervisor", Path(__file__).with_name("cargo-artifact-supervisor.py")
)
SUPERVISOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SUPERVISOR)


class SupervisorTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        root = Path(self.directory.name)
        self.supervisor = SUPERVISOR.Supervisor(root, root / "target", 1024)

    def wait_until_zombie(self, pid):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            state = subprocess.run(
                ["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True
            ).stdout.strip()
            if state.startswith("Z"):
                return
            time.sleep(0.01)
        self.fail("the guarded command did not exit")

    def test_terminating_a_group_of_only_an_unreaped_exited_command_succeeds(self):
        # A command that exits during a budget breach leaves its group holding only a zombie,
        # which macOS refuses to signal.
        process = self.supervisor._spawn_session(["sh", "-c", "exit 0"])
        self.wait_until_zombie(process.pid)

        self.assertTrue(self.supervisor.terminate_active_group())
        self.assertEqual(process.returncode, 0)


if __name__ == "__main__":
    unittest.main()
