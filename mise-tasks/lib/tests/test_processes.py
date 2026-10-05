"""Exercise task failure reporting and process-group cleanup."""

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
# The descendant ignores SIGTERM and outlives Cargo; output stays off the runner's pipes.
(trap '' TERM; exec sleep 300) >/dev/null 2>&1 &
echo $! > "$DESCENDANT_PID"
wait
"""
BUILD = "from spaceterm_tasks.cargo import build_executable; build_executable()"
CLEANUP = """
import sys, time
from pathlib import Path
from spaceterm_tasks import main
ready, cleaned = map(Path, sys.argv[1:])
def entry():
    try:
        ready.touch()
        time.sleep(30)
    finally:
        cleaned.touch()
main(entry)
"""


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


@unittest.skipUnless(os.name == "posix", "POSIX signals")
class TaskTerminationTests(unittest.TestCase):
    def test_terminated_task_runs_its_cleanup(self):
        for signum in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
            with self.subTest(signal=signum.name), tempfile.TemporaryDirectory() as directory:
                ready, cleaned = Path(directory) / "ready", Path(directory) / "cleaned"
                task = subprocess.Popen(
                    [sys.executable, "-c", CLEANUP, str(ready), str(cleaned)],
                    env={**os.environ, "PYTHONPATH": str(LIBRARY)},
                    stderr=subprocess.PIPE,
                    text=True,
                )
                try:
                    deadline = time.monotonic() + 10
                    while not ready.exists():
                        self.assertLess(time.monotonic(), deadline, "the task did not start")
                        time.sleep(0.05)
                    task.send_signal(signum)
                    _, error = task.communicate(timeout=10)
                finally:
                    if task.poll() is None:
                        task.kill()
                        task.wait()
                self.assertEqual(task.returncode, 128 + signum)
                self.assertTrue(cleaned.exists())
                self.assertEqual(error, "")


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
                self.assertEqual(task.wait(timeout=15), 128 + signal.SIGTERM)
                # SIGKILL takes effect asynchronously after stop_group escalates.
                deadline = time.monotonic() + 2
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
