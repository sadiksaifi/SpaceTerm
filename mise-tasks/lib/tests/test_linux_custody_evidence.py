"""Injected guest transports do not establish Jammy or privileged custody evidence."""

import copy
import importlib.util
import io
import signal
import subprocess
import sys
import unittest
from dataclasses import replace
from unittest.mock import patch

from spaceterm_tasks import ROOT, TaskError
from spaceterm_tasks import linux_custody_evidence as custody
from spaceterm_tasks import native_evidence_schema as schema
from spaceterm_tasks.processes import stop_group

from tests import test_server_vm_executor as vm_fixtures
from tests.test_native_evidence_schema import encoded, record


class GuestPeer:
    def __init__(self, binding, data):
        self.observation = custody.GuestObservation(
            binding.container_id,
            binding.token,
            1000,
            1001,
            (22, 4, 0),
            (5, 15, 0),
            (2, 35, 0),
            249,
            True,
            True,
            2,
        )
        self.data = data
        self.calls = []
        self.error = None
        self.cleanup_complete = True
        self.callbacks = {}
        self.stream = None

    def boundary(self, name):
        self.calls.append(name)
        if name in self.callbacks:
            self.callbacks[name]()

    def observe(self):
        self.boundary("observe")
        return self.observation

    def run_suite(self, suite):
        self.boundary("run-" + suite)
        if self.error is not None:
            raise self.error
        return self.stream if self.stream is not None else io.BytesIO(self.data)

    def cleanup_suite(self, suite):
        self.boundary("cleanup-" + suite)
        return self.cleanup_complete


class CustodyEvidenceTests(unittest.TestCase):
    def setUp(self):
        fixture = vm_fixtures.VmExecutorTests()
        fixture.new_fixture()
        self.addCleanup(fixture.doCleanups)
        self.fixture = fixture
        self.receipt = fixture.started_receipt()
        self.value = record()
        self.value.update(context="disposable-jammy")
        self.value["host"].update(
            platform="linux",
            architecture="x86_64",
            os_version=[22, 4, 0],
            kernel_version=[5, 15, 0],
            libc_version=[2, 35, 0],
            supervisor="systemd",
            supervisor_version=[249, 0, 0],
        )
        self.required = (
            schema.RequiredCase("native-installation", self.value["case"], "disposable-jammy"),
        )
        self.peer = GuestPeer(self.receipt, encoded(self.value))
        self.candidate = custody.Candidate(
            self.value["run_id"], self.value["revision"], False, self.value["artifact_sha256"]
        )
        self.driver = custody.CustodyDriver(fixture.executor, self.peer, self.candidate)

    def run_suite(self, **kwargs):
        return self.driver.collect_probe(self.receipt, "durability", self.required, **kwargs)

    def test_owned_guest_collects_observations_then_cleans_up(self):
        sink = io.BytesIO()
        collected = self.run_suite(sink=sink)
        self.assertEqual(collected.records, 1)
        self.assertEqual(
            self.peer.calls,
            ["observe", "run-durability", "observe", "cleanup-durability", "observe"],
        )
        self.assertEqual(sink.getvalue(), schema.encode_record(self.value))

    def test_unowned_nonstock_or_same_account_guest_never_runs_suite(self):
        for field, value in (
            ("token", "0" * 64),
            ("container_id", "0" * 64),
            ("administrator_uid", 1001),
            ("server_uid", 0),
            ("os_version", (24, 4, 0)),
            ("libc_version", (2, 41, 0)),
            ("systemd_version", 257),
            ("apparmor_enabled", False),
            ("apport_enabled", False),
            ("suid_dumpable", 0),
        ):
            with self.subTest(field=field):
                original = self.peer.observation
                self.peer.observation = replace(original, **{field: value})
                self.peer.calls.clear()
                with self.assertRaises(TaskError):
                    self.run_suite()
                self.assertEqual(self.peer.calls, ["observe"])
                self.peer.observation = original

    def test_foreign_container_policy_and_replaced_disk_refuse_before_transport(self):
        assert self.fixture.native.observation is not None
        config = self.fixture.native.observation.configuration
        assert config is not None
        self.fixture.native.observation = replace(
            self.fixture.native.observation, configuration=replace(config, privileged=True)
        )
        with self.assertRaises(TaskError):
            self.run_suite()
        self.assertEqual(self.peer.calls, [])

    def test_missing_schema_or_empty_requirements_refuse_before_guest_effect(self):
        for required in (
            (),
            (
                schema.RequiredCase(
                    "server-distribution", "jammy-package-execution", "disposable-jammy"
                ),
            ),
            (
                schema.RequiredCase(
                    "macos-launcher", "privilege-and-inheritance-proof", "disposable-jammy"
                ),
            ),
        ):
            with self.subTest(required=required), self.assertRaises(TaskError):
                self.driver.collect_probe(self.receipt, "packaged-execution", required)
        self.assertEqual(self.peer.calls, [])

    def test_collection_rejects_missing_synthetic_or_wrong_candidate_records(self):
        bad = []
        for field, value in (
            ("proof", "synthetic"),
            ("artifact_sha256", "9" * 64),
            ("revision", "9" * 40),
            ("run_id", "9" * 32),
            ("dirty", True),
            ("sequence", 2),
            ("result", "unrun"),
        ):
            record_value = copy.deepcopy(self.value)
            record_value[field] = value
            bad.append(encoded(record_value))
        bad.extend(
            (b"", encoded(self.value)[:-1], encoded(self.value) * 2, b'{"path":"/untrusted"}\n')
        )
        for data in bad:
            with self.subTest(length=len(data)):
                self.peer.data = data
                self.peer.calls.clear()
                with self.assertRaises(TaskError):
                    self.run_suite()
                self.assertIn("cleanup-durability", self.peer.calls)

    def test_output_failure_still_cleans_up(self):
        class BrokenSink:
            def write(self, _data):
                raise OSError("native detail must not escape")

        with self.assertRaisesRegex(TaskError, "^io-unavailable$"):
            self.run_suite(sink=BrokenSink())
        self.assertIn("cleanup-durability", self.peer.calls)

    def test_interrupted_transport_cleans_up_and_preserves_interruption(self):
        for error in (KeyboardInterrupt(), SystemExit(143)):
            self.peer.error = error
            self.peer.calls.clear()
            with self.assertRaises(type(error)):
                self.run_suite()
            self.assertIn("cleanup-durability", self.peer.calls)

    def test_cleanup_uncertainty_overrides_interruption_or_success(self):
        self.peer.cleanup_complete = False
        for error in (None, KeyboardInterrupt(), OSError("private native detail")):
            self.peer.error = error
            with self.assertRaisesRegex(TaskError, "^cleanup-unresolved$"):
                self.run_suite()

    def test_guest_replacement_after_run_is_not_credited(self):
        def substitute():
            self.peer.observation = replace(self.peer.observation, token="0" * 64)

        self.peer.callbacks["run-durability"] = substitute
        with self.assertRaisesRegex(TaskError, "^cleanup-unresolved$"):
            self.run_suite()
        self.assertNotIn("cleanup-durability", self.peer.calls)

    def test_stream_closed_and_cleanup_observed_on_decode_failure(self):
        stream = io.BytesIO(b"native stderr is not evidence\n")
        self.peer.stream = stream
        with self.assertRaisesRegex(TaskError, "^invalid-record$"):
            self.run_suite()
        self.assertTrue(stream.closed)
        self.assertIn("cleanup-durability", self.peer.calls)

    def test_cleanup_uncertainty_does_not_publish_decisive_records(self):
        sink = io.BytesIO()
        self.peer.cleanup_complete = False
        with self.assertRaisesRegex(TaskError, "^cleanup-unresolved$"):
            self.run_suite(sink=sink)
        self.assertEqual(sink.getvalue(), b"")

    def test_positive_and_denial_controls_are_both_required(self):
        denial = copy.deepcopy(self.value)
        denial.update(
            sequence=2,
            job="macos-management",
            case="authorization-denied",
            result="denied",
            failure="authorization-denied",
            facts={
                "kind": "authorization",
                "operation": "start",
                "attempt_observed": True,
                "granted": False,
                "caller_bound": False,
                "helper_absent": True,
            },
        )
        self.required += (
            schema.RequiredCase(denial["job"], denial["case"], "disposable-jammy", "denied"),
        )
        self.peer.data = encoded(self.value) + encoded(denial)
        self.assertEqual(self.run_suite().records, 2)
        for data in (encoded(self.value), encoded(denial)):
            self.peer.data = data
            with self.assertRaises(TaskError):
                self.run_suite()
        denial["facts"]["attempt_observed"] = False
        self.peer.data = encoded(self.value) + encoded(denial)
        with self.assertRaises(TaskError):
            self.run_suite()

    def test_transport_native_errors_are_not_relayed(self):
        self.peer.error = TaskError("unbounded native detail")
        with self.assertRaisesRegex(TaskError, "^native-unavailable$"):
            self.run_suite()
        self.assertIn("cleanup-durability", self.peer.calls)

    def test_vm_namespace_lease_excludes_concurrent_stop_during_transport(self):
        attempts = []
        self.peer.callbacks["run-durability"] = lambda: attempts.append(
            self.fixture.executor.stop(self.receipt)
        )
        self.run_suite()
        self.assertEqual(attempts[0].classification.value, "ownership-mismatch")
        self.assertNotIn("stop-container", [name for name, _ in self.fixture.native.calls])
        self.assertEqual(self.fixture.journal()["stage"], "started")


class CustodyProcessTests(unittest.TestCase):
    def test_handler_restoration_interrupt_retires_acquired_process(self):
        path = ROOT / "mise-tasks/server/native/linux/custody/tests.py"
        spec = importlib.util.spec_from_file_location("custody_controls", path)
        assert spec is not None and spec.loader is not None
        controls = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(controls)
        real_signal, real_popen = signal.signal, subprocess.Popen
        originals = {
            number: signal.getsignal(number)
            for number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)
        }
        for number, handler in originals.items():
            self.addCleanup(real_signal, number, handler)
        for number, failure in (
            (signal.SIGINT, KeyboardInterrupt()),
            (signal.SIGTERM, SystemExit(143)),
        ):
            with self.subTest(signal=number):
                processes, interrupted = [], []

                def original(_number, _frame):
                    raise failure

                real_signal(number, original)

                def acquire(*args, **kwargs):
                    process = real_popen(*args, **kwargs)
                    processes.append(process)
                    assert process.stdout is not None and process.stderr is not None
                    self.addCleanup(process.stdout.close)
                    self.addCleanup(process.stderr.close)
                    self.addCleanup(stop_group, process, timeout=1)
                    return process

                def restore(signum, handler):
                    previous = real_signal(signum, handler)
                    if signum == number and handler is original and not interrupted:
                        interrupted.append(True)
                        handler(signum, None)
                    return previous

                with (
                    patch.object(controls.subprocess, "Popen", side_effect=acquire),
                    patch.object(controls.signal, "signal", side_effect=restore),
                ):
                    with self.assertRaises(type(failure)) as raised:
                        controls.bounded(
                            [sys.executable, "-I", "-c", "import time; time.sleep(60)"], timeout=2
                        )
                self.assertIs(raised.exception, failure)
                self.assertEqual(interrupted, [True])
                self.assertEqual(len(processes), 1)
                process = processes[0]
                self.assertIsNotNone(process.poll())
                self.assertTrue(process.stdout.closed)
                self.assertTrue(process.stderr.closed)
                self.assertIs(signal.getsignal(number), original)
                for other, handler in originals.items():
                    if other != number:
                        self.assertIs(signal.getsignal(other), handler)
                real_signal(number, originals[number])


if __name__ == "__main__":
    unittest.main()
