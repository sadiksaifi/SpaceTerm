"""Portable regressions for the capability gate. No native operations."""

import importlib.util
import subprocess
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[4]
SPEC = importlib.util.spec_from_file_location(
    "capability_test_runner", ROOT / "mise-tasks/server/native/macos/capability_tests.py"
)
assert SPEC is not None and SPEC.loader is not None
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class CapabilityTaskTests(unittest.TestCase):
    def test_exact_executed_count_is_required(self):
        output = "\n".join(f"passed {case}" for case in runner.REQUIRED_CASES)
        output += f"\nexecuted={len(runner.REQUIRED_CASES)} proof=synthetic native-effects=0\n"
        self.assertEqual(runner.verify_test_output(output.encode()), len(runner.REQUIRED_CASES))
        for bad in [
            b"",
            b"executed=0 proof=synthetic native-effects=0\n",
            output.replace("synthetic", "native").encode(),
        ]:
            with self.assertRaises(runner.TaskError):
                runner.verify_test_output(bad)

    def test_output_sanitization_rejects_paths_native_errors_and_values(self):
        for output in [
            b"/Users/private/secret\n",
            b"NSError domain=private\n",
            b"PATH=/private/bin\n",
            b"\xff",
            b"x" * 65537,
        ]:
            with self.assertRaises(runner.TaskError):
                runner.verify_test_output(output)

    def test_signing_and_loader_inputs_do_not_reach_compiler_or_fixture(self):
        clean = runner.build_environment(
            {
                "PATH": "/private/attacker",
                "HOME": "/synthetic/home",
                "LC_ALL": "private-locale",
                "MACOS_SIGNING_CERTIFICATE_P12": "private-signing-value",
                "UPDATE_SIGNING_KEY": "private-key",
                "DYLD_INSERT_LIBRARIES": "/private/library",
                "OPENSSL_CONF": "/private/config",
                "SSH_AUTH_SOCK": "/private/socket",
            }
        )
        self.assertEqual(
            clean,
            {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LC_ALL": "C", "HOME": "/synthetic/home"},
        )

    def test_command_timeout_does_not_relay_native_output(self):
        with patch.object(
            runner.subprocess,
            "run",
            side_effect=subprocess.TimeoutExpired("private-path", 60, output=b"private-value"),
        ):
            with self.assertRaises(runner.TaskError) as caught:
                runner.checked(["/synthetic/compiler"], {})
        self.assertEqual(str(caught.exception), "capability-test-timed-out")

    def test_command_failure_does_not_relay_native_errors(self):
        result = subprocess.CompletedProcess([], 1, b"private-output", b"private-native-error")
        with patch.object(runner.subprocess, "run", return_value=result):
            with self.assertRaises(runner.TaskError) as caught:
                runner.checked(["/synthetic/compiler"], {})
        self.assertEqual(str(caught.exception), "capability-test-command-failed")

    def test_fixture_build_never_executes_or_signs_the_native_binary(self):
        destination = Path("/synthetic/output")
        with patch.object(runner, "checked", return_value=b"") as invoke:
            runner.build_fixture(destination, environment={})
        self.assertEqual(invoke.call_count, 1)
        command, environment = invoke.call_args.args
        self.assertEqual(command[:4], ["/usr/bin/xcrun", "--sdk", "macosx", "clang"])
        self.assertEqual(command[-2:], ["-o", str(destination)])
        self.assertNotIn("codesign", command)
        self.assertNotIn("security", command)
        self.assertNotIn("sudo", command)
        self.assertEqual(environment, {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LC_ALL": "C"})

    def test_sanitizer_gate_builds_both_address_and_undefined_checks(self):
        arguments = runner.compiler_arguments(sanitizers=True)
        self.assertIn("-fsanitize=address,undefined", arguments)
        self.assertIn("-fno-omit-frame-pointer", arguments)
        self.assertIn("-Werror", arguments)

    def test_failed_output_channel_is_not_a_successful_gate(self):
        result = subprocess.CompletedProcess([], 0, b"x" * 65537, b"")
        with patch.object(runner.subprocess, "run", return_value=result):
            with self.assertRaises(runner.TaskError):
                runner.checked(["/synthetic/compiler"], {})
