"""Account tasks cannot mutate by default or count empty targets as evidence."""

import io
import json
import os
import shlex
import subprocess
import sys
import tempfile
import tomllib
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import ROOT, TaskError
from spaceterm_tasks import account_service_checks as checks

from tests.test_package_server import metadata


class AccountCheckTests(unittest.TestCase):
    def test_update_entry_point_rejects_arguments_without_echoing_them(self):
        marker = "private-argument-marker"
        result = subprocess.run(
            [sys.executable, "-I", str(ROOT / "mise-tasks/test/server/update.py"), marker],
            cwd=ROOT,
            capture_output=True,
            timeout=5,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, b"")
        self.assertNotIn(marker.encode(), result.stderr)
        self.assertEqual(result.stderr, b"unsupported-update-test-arguments\n")

    def test_lifecycle_wrapper_is_removed_in_favor_of_explicit_cli_commands(self):
        self.assertFalse((ROOT / "mise-tasks/server/deployment/lifecycle.py").exists())
        with self.assertRaises(SystemExit), patch.object(checks, "test_gate") as gate:
            checks.run(["lifecycle"])
        gate.assert_not_called()

    def test_nonempty_gate_counts_only_the_selected_targets_completed_tests(self):
        output = (
            b"running 3 tests\n"
            b"running 1 test\n"
            b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; "
            b"finished in 0.01s\n"
            b"test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; "
            b"finished in 0.02s\n"
        )
        self.assertEqual(checks.require_nonempty(output), 2)

    def test_nonempty_gate_accepts_intentionally_killed_single_test_fixtures(self):
        output = (
            b"running 3 tests\n"
            b"running 1 test\n"
            b"running 1 test\n"
            b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; "
            b"finished in 0.01s\n"
            b"test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; "
            b"finished in 0.02s\n"
        )
        self.assertEqual(checks.require_nonempty(output), 3)

    def test_nonempty_gate_rejects_all_ignored_and_incomplete_targets(self):
        nested = (
            b"running 1 test\n"
            b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; "
            b"finished in 0.01s\n"
        )
        for output in (
            b"running 1 test\ni\n"
            b"test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; "
            b"finished in 0.01s\n",
            b"running 1 test\n"
            + nested
            + b"test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; "
            b"finished in 0.02s\n",
            b"running 2 tests\n" + nested,
            b"running 1 test\n" + nested,
            b"running 0 tests\n" + nested,
            b"running 1 test\n",
            b"running 1 test\n"
            b"test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; "
            b"finished in 0.01s\n",
            b"missing target harness",
            b"",
        ):
            with self.subTest(output=output), self.assertRaises(TaskError):
                checks.require_nonempty(output)

    def test_vertical_gate_runs_real_two_client_routes_and_resource_admission(self):
        output = (
            b"running 1 test\n"
            b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; "
            b"finished in 0.01s\n"
        )
        with patch.object(checks.native, "run_tool", return_value=output) as run:
            self.assertEqual(checks.run(["vertical"]), 0)
        self.assertEqual(
            [call.args[0] for call in run.call_args_list],
            [
                ["cargo", "test", "--quiet", "--locked", "-p", "spaceterm-server", "--test", target]
                for target in (
                    "authenticated_local_vertical",
                    "authenticated_route_equivalence",
                    "production_listeners",
                )
            ]
            + [
                [
                    "cargo",
                    "test",
                    "--quiet",
                    "--locked",
                    "-p",
                    "spaceterm-terminal",
                    "--test",
                    "geometry_admission",
                    "--features",
                    "runtime",
                ]
            ],
        )

    def test_vertical_gate_still_rejects_an_empty_or_failed_target(self):
        for output in (
            b"running 0 tests\n"
            b"test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; "
            b"finished in 0.01s\n",
            b"running 1 test\n"
            b"test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; "
            b"finished in 0.01s\n",
        ):
            with patch.object(checks.native, "run_tool", return_value=output):
                with self.assertRaises(TaskError):
                    checks.run(["vertical"])

    def test_bootstrap_gate_runs_native_library_and_each_assigned_integration_target(self):
        output = (
            b"running 1 test\n"
            b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; "
            b"finished in 0.01s\n"
        )
        with patch.object(checks.native, "run_tool", return_value=output) as run:
            self.assertEqual(checks.run(["bootstrap"]), 0)
        self.assertEqual(
            [call.args[0] for call in run.call_args_list],
            [
                [
                    "cargo",
                    "test",
                    "--quiet",
                    "--locked",
                    "-p",
                    "spaceterm-client-native",
                    "--lib",
                    "bootstrap::",
                ]
            ]
            + [
                ["cargo", "test", "--quiet", "--locked", "-p", package, "--test", target]
                for package, target in (
                    ("spaceterm-client", "pairing_bootstrap"),
                    ("spaceterm-host", "bootstrap_probe"),
                    ("spaceterm-host", "bootstrap_process"),
                    ("spaceterm-host", "bootstrap_linger"),
                )
            ],
        )

    def test_update_gate_requires_signed_selection_lifecycle_and_real_git_targets(self):
        output = (
            b"running 1 test\n"
            b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; "
            b"finished in 0.01s\n"
        )
        with patch.object(checks.native, "run_tool", return_value=output) as run:
            self.assertEqual(checks.run(["update"]), 0)
        self.assertEqual(
            [call.args[0] for call in run.call_args_list],
            [
                [
                    "cargo",
                    "test",
                    "--quiet",
                    "--locked",
                    "-p",
                    package,
                    "--test",
                    target,
                    "--features",
                    "test-support",
                ]
                for package, target in (
                    ("spaceterm-host", "server_installation"),
                    ("spaceterm-host", "account_service_lifecycle"),
                    ("spaceterm-host", "account_service_managers"),
                    ("spaceterm-server", "git_shutdown"),
                )
            ],
        )
        for call in run.call_args_list:
            self.assertNotIn("--ignored", call.args[0])
            self.assertNotIn("UPDATE_SIGNING_KEY", call.kwargs["environment"])
            self.assertEqual(call.kwargs["environment"]["CARGO_BUILD_JOBS"], "4")

    def test_update_gate_rejects_an_empty_target_after_a_nonempty_target(self):
        nonempty = (
            b"running 1 test\n"
            b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; "
            b"finished in 0.01s\n"
        )
        empty = (
            b"running 0 tests\n"
            b"test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; "
            b"finished in 0.01s\n"
        )
        with patch.object(checks.native, "run_tool", side_effect=[nonempty, empty]) as run:
            with self.assertRaises(TaskError):
                checks.run(["update"])
        self.assertEqual(run.call_count, 2)

    def test_bootstrap_gate_rejects_an_empty_target_after_a_nonempty_target(self):
        nonempty = (
            b"running 1 test\n"
            b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; "
            b"finished in 0.01s\n"
        )
        empty = (
            b"running 0 tests\n"
            b"test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; "
            b"finished in 0.01s\n"
        )
        with patch.object(checks.native, "run_tool", side_effect=[nonempty, empty]) as run:
            with self.assertRaises(TaskError):
                checks.run(["bootstrap"])
        self.assertEqual(run.call_count, 2)

    def test_gate_uses_exit_status_and_test_summary_not_native_warnings(self):
        summary = (
            "running 1 test\n"
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; "
            "finished in 0.01s\n"
        )
        marker = "private-native-warning-marker"
        with tempfile.TemporaryDirectory() as temporary:
            cargo = Path(temporary) / "cargo"
            for status, output, classification in (
                (0, summary, None),
                (7, summary, "tool-failed"),
                (0, "running 1 test\n", "target-incomplete"),
                (
                    0,
                    summary.replace("1 passed; 0 failed", "0 passed; 1 failed"),
                    "target-incomplete",
                ),
            ):
                with self.subTest(status=status, classification=classification):
                    cargo.write_text(
                        "#!/bin/sh\n"
                        f"printf '%s' {shlex.quote(output)}\n"
                        f"printf '%s' {shlex.quote(marker)} >&2\n"
                        f"exit {status}\n"
                    )
                    cargo.chmod(0o700)
                    stdout, stderr = io.StringIO(), io.StringIO()
                    with (
                        patch.dict(os.environ, {"PATH": f"{temporary}:/usr/bin:/bin"}),
                        patch.object(
                            checks,
                            "GATES",
                            {"account": {"spaceterm-host": ("server_installation",)}},
                        ),
                        redirect_stdout(stdout),
                        redirect_stderr(stderr),
                    ):
                        if classification is None:
                            self.assertEqual(checks.test_gate("account"), 1)
                        else:
                            with self.assertRaises(TaskError) as raised:
                                checks.test_gate("account")
                            self.assertEqual(
                                str(raised.exception),
                                "package=spaceterm-host target=server_installation "
                                f"classification={classification}",
                            )
                    self.assertNotIn(marker, stdout.getvalue() + stderr.getvalue())

    def test_account_gate_classifies_the_failed_target_without_native_output(self):
        nonempty = (
            b"running 1 test\n"
            b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; "
            b"finished in 0.01s\n"
        )
        for provider_error, expected in (
            ("tool-failed", "tool-failed"),
            ("tool-timeout", "tool-timeout"),
            ("private-path-and-environment-marker", "tool-failed"),
        ):
            with patch.object(
                checks.native,
                "run_tool",
                side_effect=[nonempty, nonempty, TaskError(provider_error)],
            ):
                with self.assertRaises(TaskError) as raised:
                    checks.run(["account"])
            self.assertEqual(
                str(raised.exception),
                f"package=spaceterm-host target=server_installation classification={expected}",
            )

    def test_startup_gate_preserves_the_callers_build_job_limit(self):
        output = (
            b"running 1 test\n"
            b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; "
            b"finished in 0.01s\n"
        )
        with (
            patch.dict(os.environ, {"CARGO_BUILD_JOBS": "2"}),
            patch.object(checks.package_server, "build_executable") as compile_fixture,
            patch.object(checks.native, "run_tool", return_value=output) as run,
        ):
            self.assertEqual(checks.test_gate("startup"), 2)
        self.assertEqual(compile_fixture.call_args.kwargs["env"]["CARGO_BUILD_JOBS"], "2")
        for call in run.call_args_list:
            self.assertEqual(call.kwargs["environment"]["CARGO_BUILD_JOBS"], "2")

    def test_startup_gate_requires_real_account_and_binary_composition_tests(self):
        output = (
            b"running 1 test\n"
            b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; "
            b"finished in 0.01s\n"
        )
        executable = Path("/approved/server")
        with (
            patch.object(checks, "check_startup") as query,
            patch.object(checks.package_server, "build") as build,
            patch.object(checks.package_server, "build_executable") as compile_fixture,
            patch.object(checks.native, "run_tool", return_value=output) as run,
        ):
            self.assertEqual(checks.run(["startup", "--executable", str(executable)]), 0)
        build.assert_not_called()
        query.assert_called_once()
        compile_fixture.assert_called_once()
        self.assertEqual(
            compile_fixture.call_args.args,
            ("--locked", "-p", "spaceterm-server", "--features", "test-support"),
        )
        self.assertEqual(compile_fixture.call_args.kwargs["binary"], "spaceterm-server")
        self.assertEqual(
            compile_fixture.call_args.kwargs["env"]["CARGO_BUILD_JOBS"],
            os.environ.get("CARGO_BUILD_JOBS", "4"),
        )
        self.assertEqual(
            [call.args[0] for call in run.call_args_list],
            [
                [
                    "cargo",
                    "test",
                    "--quiet",
                    "--locked",
                    "-p",
                    "spaceterm-server",
                    "--test",
                    target,
                    "--features",
                    "test-support",
                ]
                for target in ("account_startup", "production_listeners")
            ],
        )

    def test_startup_gate_rejects_empty_runtime_composition_evidence(self):
        empty = (
            b"running 0 tests\n"
            b"test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; "
            b"finished in 0.01s\n"
        )
        with (
            patch.object(checks, "check_startup"),
            patch.object(checks.package_server, "build_executable"),
            patch.object(checks.native, "run_tool", return_value=empty),
            self.assertRaises(TaskError),
        ):
            checks.run(["startup", "--executable", "/approved/server"])

    def test_startup_metadata_checks_run_without_serving_or_native_admission(self):
        with tempfile.TemporaryDirectory() as temporary:
            executable = Path(temporary) / "server"
            document = metadata(build="preflight")
            executable.write_text(
                '#!/bin/sh\ncase "$*" in\n'
                + "--distribution-metadata) printf '%s' "
                + shlex.quote(json.dumps(document))
                + ";;\n"
                + "--help) printf 'Usage: spaceterm-server <command>\\n';;\n"
                + "--version) printf 'spaceterm-server 1.2.3\\n';;\n"
                + "*) exit 2;;\nesac\n"
            )
            executable.chmod(0o700)
            self.assertEqual(
                checks.check_startup(executable, "linux-x86_64", "preflight"), document
            )

    def test_normal_validation_contains_no_native_evidence_or_lifecycle_mutation(self):
        tasks = tomllib.loads((ROOT / ".mise.toml").read_text())["tasks"]
        for name in tasks:
            self.assertFalse(
                name.startswith(
                    (
                        "test:server:native:",
                        "check:server:native:",
                        "server:native:",
                        "test:server:custody:",
                        "server:vm:custody:",
                    )
                ),
                name,
            )
        for gate in ("validate:portable", "validate:macos", "validate:linux"):
            names = {step["task"] for step in tasks[gate]["run"] if isinstance(step, dict)}
            self.assertIn("test:server:account", names)
            self.assertIn("test:server:vertical", names)
            self.assertIn("test:server:artifacts", names)
            self.assertIn("test:server:startup", names)
            self.assertNotIn("server:deployment:lifecycle", names)
