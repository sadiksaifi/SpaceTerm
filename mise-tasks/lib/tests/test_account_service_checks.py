"""Account tasks cannot mutate by default or count empty targets as evidence."""

import json
import shlex
import tempfile
import tomllib
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import ROOT, TaskError
from spaceterm_tasks import account_service_checks as checks

from tests.test_package_server import metadata


class AccountCheckTests(unittest.TestCase):
    def test_lifecycle_mutations_require_approval_before_any_provider_call(self):
        for operation in checks.MUTATIONS:
            with (
                self.subTest(operation=operation),
                patch.object(checks.package_server, "executable_metadata") as query,
            ):
                with self.assertRaises(TaskError):
                    checks.lifecycle(Path("/unused"), operation, "linux-x86_64", "preflight")
                query.assert_not_called()

    def test_lifecycle_uses_the_compiled_identity_and_never_wraps_spaceterm_in_sudo(self):
        with (
            patch.object(checks.os, "getuid", return_value=501),
            patch.object(checks.package_server, "executable_metadata") as query,
            patch.object(
                checks.native,
                "run_tool",
                return_value=b'{"schema":1,"result":{"kind":"native_operation","state":"Complete"}}',
            ) as run,
        ):
            checks.lifecycle(
                Path("/approved/server"), "stop", "linux-x86_64", "preflight", allow_mutation=True
            )
        query.assert_called_once_with(Path("/approved/server"), "linux-x86_64", "preflight")
        self.assertEqual(
            run.call_args.args[0], [Path("/approved/server"), "--json", "--non-interactive", "stop"]
        )

    def test_root_is_refused_before_queries_or_mutation(self):
        with (
            patch.object(checks.os, "getuid", return_value=0),
            patch.object(checks.package_server, "executable_metadata") as query,
        ):
            with self.assertRaises(TaskError):
                checks.lifecycle(
                    Path("/unused"), "start", "linux-x86_64", "preflight", allow_mutation=True
                )
            query.assert_not_called()

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

    def test_bootstrap_gate_runs_each_assigned_integration_target(self):
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
                ["cargo", "test", "--quiet", "--locked", "-p", package, "--test", target]
                for package, target in (
                    ("spaceterm-client-native", "bootstrap"),
                    ("spaceterm-client", "pairing_bootstrap"),
                    ("spaceterm-host", "bootstrap_probe"),
                    ("spaceterm-host", "bootstrap_process"),
                    ("spaceterm-host", "bootstrap_linger"),
                )
            ],
        )

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
