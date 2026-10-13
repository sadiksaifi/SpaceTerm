"""Storage evidence cannot promote a process kill into abrupt-power-loss evidence."""

import contextlib
import io
import json
import subprocess
import sys
import unittest
from unittest.mock import patch

from spaceterm_tasks import ROOT, TaskError
from spaceterm_tasks import storage_acceptance as acceptance

SUMMARY = (
    b"running 1 test\n"
    b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; "
    b"finished in 0.01s\n"
)
PROBE = {
    "filesystem": "apfs",
    "journal_mode": "delete",
    "synchronous": 2,
    "fullfsync": 1,
    "checkpoint_fullfsync": 1,
    "checkpoint": "not-applicable-delete-journal",
    "native_sync": "fsync-and-f-fullfsync",
}


def probe_output(document=PROBE):
    return b"SPACETERM_STORAGE_EVIDENCE " + json.dumps(document).encode() + b"\n" + SUMMARY


class StorageAcceptanceTests(unittest.TestCase):
    def test_entry_point_rejects_arguments_without_echoing_them(self):
        marker = "/private/account/credential-marker"
        for arguments in ([marker], ["--source-commit", marker], []):
            result = subprocess.run(
                [sys.executable, "-I", str(ROOT / "mise-tasks/test/server/storage.py"), *arguments],
                cwd=ROOT,
                capture_output=True,
                timeout=5,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, b"")
            self.assertNotIn(marker.encode(), result.stderr)
            self.assertIn(
                result.stderr,
                (
                    b"unsupported-storage-test-arguments\n",
                    b"error: invalid-storage-source-commit\n",
                ),
            )

    def test_probe_requires_a_successful_nonempty_native_test_and_exact_bounded_fields(self):
        self.assertEqual(acceptance.parse_probe(probe_output()), PROBE)
        for output in (
            SUMMARY,
            probe_output() + probe_output(),
            probe_output({**PROBE, "filesystem": "/private/sensitive-volume"}),
            probe_output({**PROBE, "filesystem": []}),
            probe_output({**PROBE, "native_sync": {}}),
            probe_output()
            .replace(b"0 ignored", b"1 ignored")
            .replace(b"running 1 test", b"running 2 tests"),
            probe_output({**PROBE, "journal_mode": "wal"}),
            probe_output({**PROBE, "synchronous": True}),
            probe_output({**PROBE, "fullfsync": 0}),
            probe_output({**PROBE, "checkpoint": "completed"}),
            probe_output({**PROBE, "native_sync": "fsync-only"}),
            probe_output({**PROBE, "path": "/private/account/store"}),
            probe_output().replace(b"1 passed", b"0 passed"),
            probe_output().replace(b"test result: ok", b"test result: FAILED"),
        ):
            with self.subTest(output=output), self.assertRaises(TaskError):
                acceptance.parse_probe(output)

    def test_linux_evidence_does_not_claim_macos_device_barriers(self):
        document = {**PROBE, "filesystem": "ext", "native_sync": "fsync-only"}
        self.assertEqual(acceptance.parse_probe(probe_output(document)), document)

    def test_invalid_candidate_is_refused_before_starting_any_tool(self):
        for candidate in ("HEAD", "a" * 7, "0" * 40, "/private/credential", "a" * 40 + "\n"):
            with (
                self.subTest(candidate=candidate),
                patch.object(acceptance.native, "run_tool") as tool,
                self.assertRaises(TaskError),
            ):
                acceptance.run(candidate)
            tool.assert_not_called()

    def test_root_or_unsupported_host_is_refused_before_any_tool(self):
        for platform, uid, euid in (("darwin", 0, 0), ("linux", 501, 0), ("win32", 501, 501)):
            with (
                self.subTest(platform=platform, uid=uid, euid=euid),
                patch.object(acceptance.sys, "platform", platform),
                patch.object(acceptance.os, "getuid", return_value=uid),
                patch.object(acceptance.os, "geteuid", return_value=euid),
                patch.object(acceptance.native, "run_tool") as tool,
                self.assertRaises(TaskError),
            ):
                acceptance.run("1" * 40)
            tool.assert_not_called()

    def test_gate_reports_provenance_and_unrun_power_loss_without_echoing_native_output(self):
        diagnostics = io.StringIO()

        def tool(command, **_kwargs):
            if command[:2] == ["git", "rev-parse"]:
                return ("2" * 40 + "\n").encode()
            if command[:3] == ["git", "status", "--porcelain"]:
                return b" M /private/sensitive-file\n"
            if (
                "storage::tests::native_filesystem_and_rollback_journal_policy_are_observed_without_reopening_database"
                in command
            ):
                return probe_output()
            return SUMMARY  # Only completed harness summaries enter evidence.

        with (
            patch("spaceterm_tasks.server_vm_linux.run_tool", side_effect=tool) as tools,
            patch("spaceterm_tasks.storage_acceptance.sys.platform", "darwin"),
            contextlib.redirect_stdout(diagnostics),
        ):
            self.assertEqual(acceptance.run("1" * 40), 0)
        document = json.loads(diagnostics.getvalue())
        self.assertEqual(document["declared_source_commit"], "1" * 40)
        self.assertEqual(document["checkout_commit"], "2" * 40)
        self.assertTrue(document["checkout_modified"])
        self.assertEqual(document["native_storage"], PROBE)
        self.assertEqual(document["abrupt_power_loss"], "not-run")
        self.assertEqual(document["installed_service"], "not-run")
        self.assertNotIn("/private", diagnostics.getvalue())
        cargo = [call.args[0] for call in tools.call_args_list if call.args[0][0] == "cargo"]
        self.assertTrue(cargo)
        self.assertTrue(all("--locked" in command for command in cargo))
        self.assertTrue(all("--ignored" not in command for command in cargo))
        self.assertTrue(all("--test-threads=1" in command for command in cargo))
        lifecycle = [command for command in cargo if "organization_lifecycle_crash" in command]
        self.assertEqual(len(lifecycle), 6)
        self.assertTrue(all("--exact" in command for command in lifecycle))
        self.assertEqual(
            {command[command.index("organization_lifecycle_crash") + 1] for command in lifecycle},
            {
                "hierarchy_crash_before_commit_retains_old_complete_state_and_original_admission",
                "hierarchy_crash_after_commit_before_application_recovers_new_complete_state_once",
                "destructive_crash_after_admission_before_acceptance_keeps_old_hierarchy_without_effect_authority",
                "destructive_crash_after_acceptance_preserves_deletion_or_ended_generations_and_pending_identity",
                "empty_retirement_crash_before_final_outcome_preserves_accepted_progress",
                "empty_retirement_crash_after_final_outcome_replays_original_revision_without_second_deletion",
            },
        )
        self.assertEqual(
            {entry["classification"] for entry in document["checks"]},
            {
                "native-sync-and-orderly-reopen",
                "orderly-reopen",
                "injected-io-fault",
                "process-kill",
                "process-kill-and-injected-io-fault",
            },
        )

    def test_gate_refuses_an_empty_target_after_native_probe_succeeds(self):
        output = io.StringIO()
        with (
            patch(
                "spaceterm_tasks.server_vm_linux.run_tool",
                side_effect=[b"2" * 40 + b"\n", b"", probe_output(), b"running 0 tests\n"],
            ),
            patch("spaceterm_tasks.storage_acceptance.sys.platform", "darwin"),
            contextlib.redirect_stdout(output),
            self.assertRaises(TaskError),
        ):
            acceptance.run("1" * 40)
        self.assertEqual(output.getvalue(), "")
