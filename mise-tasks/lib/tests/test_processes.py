"""Exercise task failure reporting and Cargo process-group cleanup."""

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

from spaceterm_tasks import ROOT, main

LIBRARY = ROOT / "mise-tasks" / "lib"
FAKE_CARGO = """#!/bin/sh
# Keep the test runner's output pipes free of the descendant.
sleep 300 >/dev/null 2>&1 &
echo $! > "$DESCENDANT_PID"
wait
"""
BUILD = "from spaceterm_tasks.cargo import build_executable; build_executable()"


def alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


class TaskFailureTests(unittest.TestCase):
    def test_operating_system_errors_are_reported_without_their_path(self):
        with tempfile.TemporaryDirectory() as directory:
            missing = Path(directory) / "private-name"
            output = io.StringIO()
            with redirect_stderr(output), self.assertRaises(SystemExit) as raised:
                main(missing.read_text)
        self.assertEqual(raised.exception.code, 1)
        self.assertEqual(
            output.getvalue(), "error: an operating-system operation failed (ENOENT)\n"
        )


@unittest.skipUnless(os.name == "posix", "POSIX process groups")
class CargoProcessGroupTests(unittest.TestCase):
    def test_terminated_build_stops_cargo_descendants(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            cargo = directory / "cargo"
            cargo.write_text(FAKE_CARGO)
            cargo.chmod(0o755)
            pid_file = directory / "descendant.pid"
            environment = {
                **os.environ,
                "PATH": f"{directory}{os.pathsep}{os.environ['PATH']}",
                "PYTHONPATH": str(LIBRARY),
                "DESCENDANT_PID": str(pid_file),
            }
            task = subprocess.Popen([sys.executable, "-c", BUILD], env=environment)
            descendant = None
            try:
                deadline = time.monotonic() + 10
                while not pid_file.exists() or not pid_file.read_text().strip():
                    self.assertLess(time.monotonic(), deadline, "Cargo did not start")
                    time.sleep(0.05)
                descendant = int(pid_file.read_text())
                task.send_signal(signal.SIGTERM)
                self.assertEqual(task.wait(timeout=10), 128 + signal.SIGTERM)
                deadline = time.monotonic() + 5
                while alive(descendant) and time.monotonic() < deadline:
                    time.sleep(0.05)
                self.assertFalse(alive(descendant))
            finally:
                if task.poll() is None:
                    task.kill()
                    task.wait()
                if descendant and alive(descendant):
                    os.kill(descendant, signal.SIGKILL)


if __name__ == "__main__":
    unittest.main()
