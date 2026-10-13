"""Content-free native storage evidence, never a power-operation provider.

Run against the frozen candidate on each host and retain stdout outside the
repository. A declared source revision is separate from the checkout revision:
penguin-run.sh creates its own sync commit. Fixture stores, real Git subprocesses
and killed children are not installed-service or abrupt-power-loss evidence.
"""

import json
import os
import re
import sys

from spaceterm_tasks import ROOT, TaskError, server_artifacts
from spaceterm_tasks import server_vm_linux as native
from spaceterm_tasks.account_service_checks import require_nonempty
from spaceterm_tasks.macos_signing import signing_environment

PROBE_TEST = "storage::tests::native_filesystem_and_rollback_journal_policy_are_observed_without_reopening_database"
PROBE_PREFIX = b"SPACETERM_STORAGE_EVIDENCE "
# Each actor case includes several real full-sync fixtures. Bound each case on a
# shared build host rather than treating a slow multi-case suite as a lost result.
LIFECYCLE_TESTS = (
    "hierarchy_crash_before_commit_retains_old_complete_state_and_original_admission",
    "hierarchy_crash_after_commit_before_application_recovers_new_complete_state_once",
    "destructive_crash_after_admission_before_acceptance_keeps_old_hierarchy_without_effect_authority",
    "destructive_crash_after_acceptance_preserves_deletion_or_ended_generations_and_pending_identity",
    "empty_retirement_crash_before_final_outcome_preserves_accepted_progress",
    "empty_retirement_crash_after_final_outcome_replays_original_revision_without_second_deletion",
)
TARGETS = (
    ("spaceterm-host", ["--lib", "storage::tests::"], "injected-io-fault"),
    ("spaceterm-host", ["--test", "organization_store"], "orderly-reopen"),
    ("spaceterm-host", ["--test", "organization_store_recovery"], "orderly-reopen"),
    ("spaceterm-host", ["--test", "organization_store_faults"], "injected-io-fault"),
    ("spaceterm-host", ["--test", "organization_store_crash"], "process-kill"),
    *(
        ("spaceterm-server", ["--test", "organization_lifecycle_crash", test], "process-kill")
        for test in LIFECYCLE_TESTS
    ),
    (
        "spaceterm-server",
        ["--test", "external_write_recovery"],
        "process-kill-and-injected-io-fault",
    ),
)


def completed_tests(output):
    count = require_nonempty(output)
    # Ignored tests must never silently narrow acceptance on a different host.
    summary = re.findall(rb"test result: ok\. .*", output)[-1]
    if b"; 0 ignored; 0 measured;" not in summary:
        raise TaskError("storage-acceptance-tests-not-executed")
    return count


def parse_probe(output):
    completed_tests(output)
    records = [
        line[len(PROBE_PREFIX) :] for line in output.splitlines() if line.startswith(PROBE_PREFIX)
    ]
    if len(records) != 1:
        raise TaskError("native-storage-probe-missing")
    document = server_artifacts.strict_json(records[0], 4096)
    if not isinstance(document, dict) or set(document) != {
        "filesystem",
        "journal_mode",
        "synchronous",
        "fullfsync",
        "checkpoint_fullfsync",
        "checkpoint",
        "native_sync",
    }:
        raise TaskError("invalid-native-storage-evidence")
    if (
        type(document["filesystem"]) is not str
        or document["filesystem"] not in {"apfs", "hfs", "ext", "xfs", "btrfs"}
        or document["journal_mode"] != "delete"
        or document["checkpoint"] != "not-applicable-delete-journal"
        or any(
            type(document[key]) is not int or document[key] != expected
            for key, expected in (("synchronous", 2), ("fullfsync", 1), ("checkpoint_fullfsync", 1))
        )
        or document["native_sync"]
        != ("fsync-and-f-fullfsync" if document["filesystem"] in {"apfs", "hfs"} else "fsync-only")
    ):
        raise TaskError("invalid-native-storage-evidence")
    return document


def run(source_commit):
    if (
        not isinstance(source_commit, str)
        or re.fullmatch(r"[0-9a-f]{40}", source_commit) is None
        or source_commit == "0" * 40
    ):
        raise TaskError("invalid-storage-source-commit")
    if sys.platform not in {"darwin", "linux"}:
        raise TaskError("unsupported-storage-acceptance-host")
    if os.getuid() == 0 or os.geteuid() == 0:
        raise TaskError("storage-acceptance-account-required")
    environment = signing_environment(os.environ)
    environment["CARGO_BUILD_JOBS"] = "4"
    checkout = native.run_tool(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, environment=environment, maximum=128
    )
    if re.fullmatch(rb"[0-9a-f]{40}\n", checkout) is None:
        raise TaskError("invalid-storage-checkout-commit")
    modified = bool(
        native.run_tool(
            ["git", "status", "--porcelain", "--untracked-files=normal"],
            cwd=ROOT,
            environment=environment,
        )
    )
    checks = []

    def execute(package, selector, classification):
        command = [
            "cargo",
            "test",
            "--quiet",
            "--locked",
            "-p",
            package,
            "--features",
            "test-support",
            *selector,
            "--",
            "--nocapture",
            "--test-threads=1",
        ]
        if len(selector) == 3 or selector[-1] == PROBE_TEST:
            command.append("--exact")
        try:
            output = native.run_tool(
                command, cwd=ROOT, environment=environment, timeout=300, maximum=1024 * 1024
            )
        except TaskError:
            # Only fixed suite/case names enter diagnostics, never tool output.
            raise TaskError(
                f"storage-acceptance-target-failed ({package}/{selector[-1]})"
            ) from None
        checks.append(
            {
                "package": package,
                "target": selector[-1],
                "classification": classification,
                "tests": completed_tests(output),
                "result": "passed",
            }
        )
        return output

    native_storage = parse_probe(
        execute("spaceterm-host", ["--lib", PROBE_TEST], "native-sync-and-orderly-reopen")
    )
    if (native_storage["filesystem"] in {"apfs", "hfs"}) != (sys.platform == "darwin"):
        raise TaskError("native-storage-host-mismatch")
    for package, selector, classification in TARGETS:
        execute(package, selector, classification)
    print(
        json.dumps(
            {
                "schema": 1,
                "declared_source_commit": source_commit,
                "checkout_commit": checkout.decode("ascii").strip(),
                "checkout_modified": modified,
                "platform": sys.platform,
                "native_storage": native_storage,
                "checks": checks,
                "abrupt_power_loss": "not-run",
                "installed_service": "not-run",
                "result": "passed",
            },
            sort_keys=True,
        )
    )
    return 0
