"""Regressions for unpublished witnesses and cancellation result precedence."""

import importlib.util
import os
import signal
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[4]
SOURCE = ROOT / "mise-tasks/server/native/macos/launcher_tests.py"


def launcher_module():
    spec = importlib.util.spec_from_file_location("_launcher_cleanup_tests", SOURCE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class UnpublishedWitnessTests(unittest.TestCase):
    def probe(self, failure, *, retirement_failure=False):
        module = launcher_module()
        driver = module.supervisor()
        descriptors = []
        original_pipe = os.pipe
        original_select = module.select.select

        def pipe():
            pair = original_pipe()
            descriptors.extend(pair)
            return pair

        def select(read, write, errors, timeout):
            if retirement_failure:
                raise OSError("injected retirement observation failure")
            return original_select(read, write, errors, min(timeout, 0.01))

        checkpoint = lambda: None
        spawn_failure = OSError("injected creation failure")
        if failure == "signal":

            def checkpoint():
                raise driver.Terminated(signal.SIGTERM)

        observed = None
        with (
            patch.object(module, "CANCELLATION_CASES", (("pre-popen", "exec", signal.SIGTERM),)),
            patch.object(module.os, "pipe", side_effect=pipe),
            patch.object(module.select, "select", side_effect=select),
            patch.object(driver, "_cancellation_checkpoint", side_effect=checkpoint),
            patch.object(module.subprocess, "Popen", side_effect=spawn_failure) as spawn,
        ):
            try:
                module.cancellation_cases(driver, Path("/unexecuted"), {})
            except BaseException as error:
                observed = error
        closed = []
        for fd in descriptors:
            try:
                os.fstat(fd)
            except OSError:
                closed.append(True)
            else:
                closed.append(False)
                os.close(fd)
        return SimpleNamespace(error=observed, closed=closed, calls=spawn.call_count, driver=driver)

    def test_pre_popen_cancellation_preserves_signal_and_closes_both_endpoints(self):
        result = self.probe("signal")
        self.assertIsInstance(result.error, result.driver.Terminated)
        self.assertEqual(result.error.number, signal.SIGTERM)
        self.assertEqual(result.calls, 0)
        self.assertEqual(result.closed, [True, True])

    def test_popen_failure_preserves_creation_error_and_closes_both_endpoints(self):
        result = self.probe("spawn")
        self.assertIsInstance(result.error, OSError)
        self.assertEqual(result.calls, 1)
        self.assertEqual(result.closed, [True, True])

    def test_no_child_requires_no_retirement_observation(self):
        result = self.probe("signal", retirement_failure=True)
        self.assertIsInstance(result.error, result.driver.Terminated)
        self.assertEqual(result.closed, [True, True])

    def test_retirement_failure_still_closes_pipe_and_stdout(self):
        module = launcher_module()
        driver = module.supervisor()
        descriptors = []
        original_pipe = os.pipe
        from unittest.mock import Mock

        process = SimpleNamespace(poll=lambda: None, stdout=Mock())

        def pipe():
            pair = original_pipe()
            descriptors.extend(pair)
            return pair

        with (
            patch.object(module, "CANCELLATION_CASES", (("cleanup", "exec", signal.SIGTERM),)),
            patch.object(module.os, "pipe", side_effect=pipe),
            patch.object(module.select, "select", return_value=([], [], [])),
            patch.object(driver, "_cancellation_checkpoint"),
            patch.object(module.subprocess, "Popen", return_value=process),
            patch.object(
                driver, "stop_process", side_effect=driver.TaskFailure("cleanup-unresolved")
            ),
        ):
            with self.assertRaises(driver.TaskFailure) as raised:
                module.cancellation_cases(driver, Path("/unexecuted"), {})
        closed = []
        for fd in descriptors:
            try:
                os.fstat(fd)
            except OSError:
                closed.append(True)
            else:
                closed.append(False)
                os.close(fd)
        self.assertEqual(raised.exception.classification, "cleanup-unresolved")
        self.assertEqual(closed, [True, True])
        process.stdout.close.assert_called_once()

    def test_pre_popen_signal_removes_the_owned_build_directory(self):
        module = launcher_module()
        driver = module.supervisor()
        directories = []
        original_mkdtemp = module.tempfile.mkdtemp

        def directory(*args, **kwargs):
            value = original_mkdtemp(*args, **kwargs)
            directories.append(Path(value))
            return value

        records = [
            dict(schema=1, scope=driver.SCOPE, case=name, result="pass") for name in module.CASES
        ]
        with (
            patch.object(module.sys, "platform", "darwin"),
            patch.object(module.tempfile, "mkdtemp", side_effect=directory),
            patch.object(driver, "bounded_process", return_value=(0, b"")),
            patch.object(driver, "validate_records", return_value=records),
            patch.object(driver, "emit"),
            patch.object(
                driver, "_cancellation_checkpoint", side_effect=driver.Terminated(signal.SIGTERM)
            ),
            patch.object(module.subprocess, "Popen") as spawn,
        ):
            with self.assertRaises(driver.Terminated) as raised:
                module.run_task(driver, execute=True)
        self.assertEqual(raised.exception.number, signal.SIGTERM)
        self.assertEqual(len(directories), 1)
        self.assertFalse(directories[0].exists())
        spawn.assert_not_called()
