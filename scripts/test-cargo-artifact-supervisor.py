#!/usr/bin/env python3
"""Test the Cargo artifact supervisor's control of its guarded process group."""

import importlib.util
import io
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from unittest.mock import patch

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

    def test_long_running_command_reports_bounded_artifact_progress(self):
        stderr = io.StringIO()
        with patch.object(SUPERVISOR, "HEARTBEAT_INTERVAL_SECONDS", 0.05, create=True):
            with redirect_stderr(stderr):
                status = self.supervisor.run(["/bin/sleep", "0.3"])
        self.assertEqual(status, 0)
        self.assertIn("guarded command active", stderr.getvalue())
        self.assertIn("target usage", stderr.getvalue())

    def test_failed_command_with_a_lingering_child_returns_its_failure(self):
        self.assert_lingering_command_status(37, 37)

    def test_successful_command_with_a_lingering_child_fails(self):
        self.assert_lingering_command_status(0, 2)

    def assert_lingering_command_status(self, exit_code, expected_status):
        root = Path(self.directory.name)
        pid_file = root / "command-pid"
        command = root / "command.py"
        command.write_text(
            "import os, pathlib, subprocess, sys\n"
            "subprocess.Popen(['/bin/sleep', '30'])\n"
            "pathlib.Path(sys.argv[1]).write_text(str(os.getpid()))\n"
            f"sys.exit({exit_code})\n"
        )
        process = subprocess.Popen(
            [
                sys.executable,
                str(Path(__file__).with_name("cargo-artifact-supervisor.py")),
                "--repo-dir", str(root), "--target-dir", str(root / "target"),
                "--budget-kib", "1024", "--", sys.executable, str(command), str(pid_file),
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            stderr = process.communicate(timeout=6)[1]
            self.assertEqual(process.returncode, expected_status, stderr)
            self.assertIn("lingering process group", stderr)
            leader = str(int(pid_file.read_text()))
            group = subprocess.run(
                ["ps", "-axo", "pgid=,stat="], check=True, capture_output=True, text=True
            ).stdout
            self.assertFalse(
                any(
                    fields[0] == leader and not fields[1].startswith("Z")
                    for line in group.splitlines()
                    if len(fields := line.split()) >= 2
                )
            )
        finally:
            if pid_file.exists():
                try:
                    os.killpg(int(pid_file.read_text()), signal.SIGKILL)
                except ProcessLookupError:
                    pass
            if process.poll() is None:
                process.kill()
            process.wait(timeout=5)
            process.stderr.close()


if __name__ == "__main__":
    unittest.main()
