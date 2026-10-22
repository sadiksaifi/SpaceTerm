"""Exercise ownership with real child processes and private IPC."""

import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

from spaceterm_tasks import LIBRARY

SERVER = """import pathlib, signal, sys, time
root = pathlib.Path(sys.argv[1])
def stopped(*_):
    (root / "server-stopped").touch()
    raise SystemExit(0)
signal.signal(signal.SIGTERM, stopped)
(root / "server-started").touch()
print("spaceterm-development-ready-v1", flush=True)
sys.stdin.buffer.read()
(root / "server-stopped").touch()
"""
CLIENT = """import os, pathlib, socket, sys
root = pathlib.Path(sys.argv[1])
connection = socket.socket(socket.AF_UNIX)
connection.connect(os.environ["SPACETERM_DEVELOPMENT_OWNER_SOCKET"])
(root / "client-started").touch()
connection.recv(1)
(root / "client-stopped").touch()
"""
# Advance only explicit process-wait deadlines. Children, pipes, signals and unbounded
# waits remain real, so a gated cleanup can cross any launcher deadline without a long sleep.
FAST_DEADLINES = """import subprocess
RealProcess = subprocess.Popen
class Process(RealProcess):
    def wait(self, timeout=None):
        return super().wait(timeout=None if timeout is None else min(timeout, 0.01))
subprocess.Popen = Process
"""
GATED_SERVER = """import pathlib, signal, sys, time
root = pathlib.Path(sys.argv[1])
def forced_stop(*_):
    (root / "forced-stop").touch()
    raise SystemExit(3)
signal.signal(signal.SIGTERM, forced_stop)
(root / "server-started").touch()
print("spaceterm-development-ready-v1", flush=True)
sys.stdin.buffer.read()
(root / "cleanup-started").touch()
while not (root / "release-cleanup").exists():
    time.sleep(0.01)
(root / "server-stopped").touch()
"""
DRIVER = """import os, sys
from spaceterm_tasks.development_supervision import supervise
supervise([sys.executable, sys.argv[1], sys.argv[3]],
          [sys.executable, sys.argv[2], sys.argv[3]], os.environ)
"""


class DevelopmentSupervisionTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.server = self.root / "server.py"
        self.server.write_text(SERVER)
        self.client = self.root / "client.py"
        self.client.write_text(CLIENT)

    def start(self, fast_deadlines=False):
        driver = (FAST_DEADLINES if fast_deadlines else "") + DRIVER
        process = subprocess.Popen(
            [sys.executable, "-c", driver, str(self.server), str(self.client), str(self.root)],
            env={**os.environ, "PYTHONPATH": str(LIBRARY)},
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.addCleanup(self.reap, process)
        return process

    def reap(self, process):
        if process.poll() is None:
            process.terminate()
        process.communicate(timeout=15)

    def await_file(self, name, process):
        deadline = time.monotonic() + 10
        while not (self.root / name).exists():
            if process.poll() is not None:
                self.fail(str(process.communicate()))
            self.assertLess(time.monotonic(), deadline, name)
            time.sleep(0.02)

    def test_ctrl_c_disconnects_owned_client_and_gracefully_stops_server(self):
        process = self.start()
        self.await_file("client-started", process)
        process.send_signal(signal.SIGINT)
        process.communicate(timeout=15)
        self.assertTrue((self.root / "client-stopped").exists())
        self.assertTrue((self.root / "server-stopped").exists())

    def test_client_exit_stops_server(self):
        self.client.write_text(CLIENT.replace("connection.recv(1)", "connection.close()"))
        process = self.start()
        _, stderr = process.communicate(timeout=15)
        self.assertEqual(process.returncode, 0, stderr)
        self.assertTrue((self.root / "server-stopped").exists())

    def test_server_failure_does_not_launch_client(self):
        self.server.write_text("raise SystemExit(1)\n")
        process = self.start()
        process.communicate(timeout=15)
        self.assertNotEqual(process.returncode, 0)
        self.assertFalse((self.root / "client-started").exists())

    def test_interrupt_during_partial_readiness_cleans_up_spawned_server(self):
        self.server.write_text(
            SERVER.replace(
                'print("spaceterm-development-ready-v1", flush=True)',
                'sys.stdout.write("s"); sys.stdout.flush()',
            )
        )
        process = self.start()
        self.await_file("server-started", process)
        process.send_signal(signal.SIGINT)
        process.communicate(timeout=15)
        self.assertFalse((self.root / "client-started").exists())
        self.assertTrue((self.root / "server-stopped").exists())

    def test_partial_readiness_has_a_deadline_and_stops_only_its_child(self):
        self.server.write_text(
            SERVER.replace(
                'print("spaceterm-development-ready-v1", flush=True)',
                'sys.stdout.write("s"); sys.stdout.flush()',
            )
        )
        unrelated = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
        self.addCleanup(self.reap, unrelated)
        process = self.start()
        process.communicate(timeout=15)
        self.assertNotEqual(process.returncode, 0)
        self.assertFalse((self.root / "client-started").exists())
        self.assertTrue((self.root / "server-stopped").exists())
        self.assertIsNone(unrelated.poll())

    def test_server_exit_disconnects_client(self):
        self.server.write_text(
            SERVER.replace(
                "sys.stdin.buffer.read()",
                'while not (root / "client-started").exists(): time.sleep(0.01)',
            )
        )
        process = self.start()
        process.communicate(timeout=15)
        self.assertNotEqual(process.returncode, 0)
        self.assertTrue((self.root / "client-stopped").exists())

    def test_concurrent_run_is_refused_without_replacing_the_owner(self):
        from spaceterm_tasks import TaskError
        from spaceterm_tasks.development_supervision import development_run

        with development_run(self.root):
            with self.assertRaisesRegex(TaskError, "another source Development run"):
                with development_run(self.root):
                    self.fail("a second owner acquired the account lock")
        with development_run(self.root):
            pass

    def test_interrupt_waits_for_gated_cleanup_beyond_process_wait_deadlines(self):
        self.server.write_text(GATED_SERVER)
        process = self.start(fast_deadlines=True)
        try:
            self.await_file("client-started", process)
            process.send_signal(signal.SIGINT)
            self.await_file("cleanup-started", process)
            with self.assertRaises(subprocess.TimeoutExpired):
                process.wait(timeout=0.25)
            self.assertFalse((self.root / "forced-stop").exists())
        finally:
            (self.root / "release-cleanup").touch()
        process.communicate(timeout=10)
        self.assertTrue((self.root / "server-stopped").exists())

    def test_unconsumed_readiness_does_not_authorize_forced_server_cleanup(self):
        self.server.write_text(
            GATED_SERVER.replace(
                'print("spaceterm-development-ready-v1", flush=True)',
                'sys.stdout.write("s"); sys.stdout.flush()',
            )
        )
        process = self.start(fast_deadlines=True)
        try:
            self.await_file("server-started", process)
            process.send_signal(signal.SIGINT)
            self.await_file("cleanup-started", process)
            with self.assertRaises(subprocess.TimeoutExpired):
                process.wait(timeout=0.25)
            self.assertFalse((self.root / "forced-stop").exists())
            self.assertFalse((self.root / "client-started").exists())
        finally:
            (self.root / "release-cleanup").touch()
        process.communicate(timeout=10)
        self.assertTrue((self.root / "server-stopped").exists())
