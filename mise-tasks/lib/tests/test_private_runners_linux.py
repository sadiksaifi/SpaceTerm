"""Test private runner interruption without launching a bus or display."""

import ctypes
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

from spaceterm_tasks import ROOT

ACCESSIBILITY = ROOT / "mise-tasks/test/accessibility/linux.py"
COMPOSE = ROOT / "mise-tasks/test/compose/linux.py"
FAKE_SESSION = '''import json, os, pathlib, signal, subprocess, sys, time
null = os.open(os.devnull, os.O_WRONLY)
os.dup2(null, 1)
os.dup2(null, 2)
os.close(null)
record = pathlib.Path(os.environ["RUNNER_TEST_RECORD"])
ready = record.with_suffix(".ready")
child = subprocess.Popen([sys.executable, "-c", """
import pathlib, signal, sys, time
signal.signal(signal.SIGTERM, signal.SIG_IGN)
pathlib.Path(sys.argv[1]).touch()
time.sleep(60)
""", str(ready)], start_new_session=os.environ["RUNNER_TEST_KIND"] == "accessibility")
display = None
lock = record.with_suffix(".lock")
if os.environ["RUNNER_TEST_KIND"] == "compose":
    display = subprocess.Popen([sys.executable, "-c", """
import pathlib, signal, sys, time
lock = pathlib.Path(sys.argv[1])
def interrupted(_signal, _frame):
    lock.unlink()
    raise SystemExit(0)
signal.signal(signal.SIGTERM, interrupted)
lock.touch()
time.sleep(60)
""", str(lock)])
while not ready.exists() or (display is not None and not lock.exists()):
    time.sleep(0.01)
pending = record.with_suffix(".next")
pending.write_text(json.dumps({"wrapper": os.getpid(), "child": child.pid,
                             "runtime": os.environ["XDG_RUNTIME_DIR"],
                             "home": os.environ["HOME"], "display": None if display is None else display.pid}))
pending.replace(record)
time.sleep(60)
'''


@unittest.skipUnless(sys.platform == "linux", "private runners require Linux")
class PrivateRunnerTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.commands = self.root / "commands"
        self.commands.mkdir()
        command = self.commands / "dbus-run-session"
        command.write_text(f"#!{sys.executable}\n" + FAKE_SESSION)
        command.chmod(0o755)
        self.record = self.root / "session.json"
        self.environment = dict(os.environ)
        for key in (
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "DBUS_SESSION_BUS_ADDRESS",
            "DBUS_SYSTEM_BUS_ADDRESS",
            "XDG_RUNTIME_DIR",
        ):
            self.environment.pop(key, None)
        self.environment.update(
            PATH=str(self.commands) + os.pathsep + os.environ["PATH"],
            RUNNER_TEST_RECORD=str(self.record),
        )
        # Adopt fixture orphans so cleanup can prove their identities are gone.
        self.libc = ctypes.CDLL(None, use_errno=True)
        previous = ctypes.c_int()
        self.assertEqual(self.libc.prctl(37, ctypes.byref(previous), 0, 0, 0), 0)
        self.assertEqual(self.libc.prctl(36, 1, 0, 0, 0), 0)
        self.addCleanup(self.libc.prctl, 36, previous.value, 0, 0, 0)

    def run_interrupted(self, kind, signum):
        self.environment["RUNNER_TEST_KIND"] = kind
        command = [
            sys.executable,
            str(ACCESSIBILITY),
            "--backend",
            "x11",
            "--isolation-only",
            "--output-dir",
            str(self.root / "output"),
        ]
        if kind == "compose":
            command = [sys.executable, str(COMPOSE), "x11"]
            if signum is None:
                # Shorten only the external session deadline. Exercise the real wait and timeout.
                command = [
                    sys.executable,
                    "-c",
                    """
import importlib.util, subprocess, sys
spec = importlib.util.spec_from_file_location("compose", sys.argv[1])
compose = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compose)
wait = subprocess.Popen.wait
subprocess.Popen.wait = lambda self, timeout=None: wait(self, 2 if timeout is not None and timeout > 1000 else timeout)
sys.argv = [sys.argv[1], "x11"]
raise SystemExit(compose.main())
""",
                    str(COMPOSE),
                ]
        process = subprocess.Popen(
            command,
            env=self.environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            preexec_fn=lambda: signal.signal(signal.SIGHUP, signal.SIG_DFL),
        )
        identities = {}
        keys = ("wrapper", "child", "display") if kind == "compose" else ("wrapper", "child")
        descriptors = {}
        stop = threading.Event()
        reaper = None
        try:
            deadline = time.monotonic() + 5
            while (
                not self.record.exists() and process.poll() is None and time.monotonic() < deadline
            ):
                time.sleep(0.01)
            self.assertTrue(self.record.exists(), "the controlled private session must start")
            identities = json.loads(self.record.read_text())
            for key in keys:
                descriptors[key] = os.pidfd_open(identities[key])

            def reap():
                while not stop.is_set():
                    for key in keys:
                        try:
                            os.waitpid(identities[key], os.WNOHANG)
                        except ChildProcessError:
                            pass
                    time.sleep(0.01)

            reaper = threading.Thread(target=reap)
            reaper.start()
            if signum is not None:
                process.send_signal(signum)
            _, error = process.communicate(timeout=15)
            self.assertNotEqual(process.returncode, 0, error)
            runtime = Path(identities["runtime"])
            self.assertFalse(runtime.exists(), "interruption must remove the private runtime")
            if kind == "compose":
                self.assertFalse(
                    self.record.with_suffix(".lock").exists(),
                    "the private display must receive time to remove its lock",
                )
            if kind == "accessibility":
                proof = json.loads((Path(identities["home"]).parent / "cleanup.json").read_text())
                self.assertTrue(proof["passed"], proof)
            elif signum is not None:
                self.assertEqual(process.returncode, 128 + signum, error)
            deadline = time.monotonic() + 5
            while any(Path(f"/proc/{identities[key]}").exists() for key in keys):
                if time.monotonic() >= deadline:
                    break
                time.sleep(0.01)
            for key in keys:
                with self.assertRaises(ProcessLookupError, msg=key):
                    os.kill(identities[key], 0)
        finally:
            if process.poll() is None:
                process.kill()
            # Also clean the unfixed runner during the red phase.
            for descriptor in descriptors.values():
                try:
                    signal.pidfd_send_signal(descriptor, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                finally:
                    os.close(descriptor)
            process.communicate(timeout=5)
            if reaper is not None:
                stop.set()
                reaper.join(timeout=5)
            for key in keys:
                if key in identities:
                    try:
                        os.waitpid(identities[key], 0)
                    except ChildProcessError:
                        pass
            if identities:
                shutil.rmtree(identities["runtime"], ignore_errors=True)

    def test_accessibility_sigterm_cleans_detached_session(self):
        self.run_interrupted("accessibility", signal.SIGTERM)

    def test_accessibility_sighup_cleans_detached_session(self):
        self.run_interrupted("accessibility", signal.SIGHUP)

    def test_compose_sigterm_cleans_the_process_group(self):
        self.run_interrupted("compose", signal.SIGTERM)

    def test_compose_sighup_cleans_the_process_group(self):
        self.run_interrupted("compose", signal.SIGHUP)

    def test_compose_timeout_cleans_the_process_group(self):
        self.run_interrupted("compose", None)


if __name__ == "__main__":
    unittest.main()
