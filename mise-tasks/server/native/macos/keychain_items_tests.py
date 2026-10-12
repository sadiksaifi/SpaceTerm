#!/usr/bin/env -S python -I
# MISE description="Test guarded macOS Keychain calls with injected item operations only"
"""Build ordinary fixtures without opening, enrolling, or removing Keychain items."""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / "mise-tasks" / "lib"))

from spaceterm_tasks import TaskError, main  # noqa: E402
from spaceterm_tasks.macos_signing import signing_environment  # noqa: E402

CASES = frozenset(
    (
        "persisted_policy_rejects_alternate_decrypt_grant",
        "persisted_policy_rejects_prompt_fallback",
        "persisted_policy_rejects_wrong_retriever_requirement",
        "persisted_item_rejects_wrong_account_incarnation_and_slot",
        "unsupported_auxiliary_acl_is_not_ignored",
        "interaction_required_retrieval_has_no_prompt_retry",
        "policy_inspection_never_reads_secret",
        "retrieval_requires_live_release_permit",
        "framework_secret_allocation_is_wiped_on_every_exit",
        "ordinary_tests_never_open_or_mutate_system_keychain",
        "persisted_owner_rejects_root_without_code_conjunction",
        "persisted_policy_rejects_delegate_grants",
        "persistent_identity_is_not_a_matching_label",
        "native_guard_precedes_all_keychain_calls",
        "release_rechecked_after_retrieval_before_copy",
        "policy_and_identity_rechecked_after_secret_read",
        "framework_plaintext_is_released_before_reinspection",
        "staging_lock_failure_prevents_secret_read",
        "staging_unlock_failure_wipes_destination",
        "fixed_item_rejects_alternate_attributes_and_class",
        "unavailable_public_retrieval_wipes_destination",
    )
)


def unique_object(pairs: list[tuple[str, object]]) -> dict[str, object]:
    result: dict[str, object] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError
        result[key] = value
    return result


def executed_counts(output: bytes) -> tuple[int, int]:
    """Require each allowlisted result and an exact final count; no skip passes."""
    if len(output) > 65536:
        raise TaskError("keychain-tests-invalid-output")
    seen: set[str] = set()
    failed = 0
    summary = False
    try:
        for line in output.decode("utf-8").splitlines():
            record = json.loads(line, object_pairs_hook=unique_object)
            if (
                type(record) is not dict
                or type(record.get("schema")) is not int
                or record.get("schema") != 1
                or record.get("scope") != "injected-keychain"
                or summary
            ):
                raise ValueError
            if "summary" in record:
                if (
                    set(record) != {"schema", "scope", "summary", "cases", "failed"}
                    or record["summary"] is not True
                    or type(record["cases"]) is not int
                    or type(record["failed"]) is not int
                    or record["cases"] != len(CASES)
                    or record["failed"] != failed
                ):
                    raise ValueError
                summary = True
            else:
                case = record.get("case")
                if (
                    set(record) != {"schema", "scope", "case", "passed"}
                    or type(case) is not str
                    or case not in CASES
                    or case in seen
                    or type(record["passed"]) is not bool
                ):
                    raise ValueError
                seen.add(case)
                failed += not record["passed"]
        if not summary or seen != CASES:
            raise ValueError
    except (UnicodeError, ValueError, TypeError):
        raise TaskError("keychain-tests-invalid-output") from None
    return len(seen), failed


def run_task(*, sanitizers: bool = False) -> int:
    if sys.platform != "darwin":
        raise TaskError("keychain-tests-unavailable")
    environment = signing_environment(os.environ)
    sources = ROOT / "tests" / "server_native" / "macos"
    native = ROOT / "crates" / "spaceterm-host" / "native" / "macos"
    with tempfile.TemporaryDirectory(prefix="spaceterm-keychain-tests-") as directory:
        binary = Path(directory) / "keychain-tests"
        production = Path(directory) / "keychain-items.o"
        compiler = [
            "/usr/bin/xcrun",
            "--sdk",
            "macosx",
            "clang",
            "-std=c11",
            "-arch",
            "arm64",
            "-mmacosx-version-min=26.0",
            "-Wall",
            "-Wextra",
            "-Werror",
        ]
        command = [
            *compiler,
            "-DST_KEYCHAIN_TESTING",
            str(sources / "keychain_items.c"),
            str(sources / "keychain_items_tests.c"),
            "-framework",
            "Security",
            "-framework",
            "CoreFoundation",
            "-o",
            str(binary),
        ]
        if sanitizers:
            command.extend(["-g", "-O1", "-fsanitize=address,undefined", "-fno-omit-frame-pointer"])
        try:
            # Compile the production translation unit independently. This is not
            # evidence of a usable privileged binding or persisted enforcement.
            compiled = subprocess.run(
                [*compiler, "-c", str(native / "keychain_items.c"), "-o", str(production)],
                cwd=ROOT,
                env=environment,
                timeout=60,
            )
            if compiled.returncode:
                return compiled.returncode
            compiled = subprocess.run(command, cwd=ROOT, env=environment, timeout=60)
            if compiled.returncode:
                return compiled.returncode
            production_symbols = subprocess.run(
                ["/usr/bin/nm", "-gj", str(production)],
                cwd=ROOT,
                env=environment,
                timeout=10,
                capture_output=True,
            )
            fixture_symbols = subprocess.run(
                ["/usr/bin/nm", "-u", str(binary)],
                cwd=ROOT,
                env=environment,
                timeout=10,
                capture_output=True,
            )
            if (
                production_symbols.returncode != 0
                or fixture_symbols.returncode != 0
                or b"st_keychain_test_" in production_symbols.stdout
                or b"_SecKeychain" in fixture_symbols.stdout
                or b"_SecItem" in fixture_symbols.stdout
            ):
                raise TaskError("keychain-tests-composition-rejected")
            result = subprocess.run(
                [str(binary)],
                cwd=ROOT,
                env=environment,
                timeout=30,
                stdout=subprocess.PIPE,
            )
            count, failed = executed_counts(result.stdout)
            print(f"proof=synthetic cases={count} failed={failed}")
            return 0 if result.returncode == 0 and failed == 0 else 1
        except subprocess.TimeoutExpired:
            raise TaskError("keychain-tests-timeout") from None


class OutputTests(unittest.TestCase):
    def output(self) -> bytes:
        records = [
            {"schema": 1, "scope": "injected-keychain", "case": case, "passed": True}
            for case in sorted(CASES)
        ]
        records.append(
            {
                "schema": 1,
                "scope": "injected-keychain",
                "summary": True,
                "cases": len(CASES),
                "failed": 0,
            }
        )
        return "\n".join(json.dumps(record) for record in records).encode()

    def test_exact_executed_counts(self):
        self.assertEqual(executed_counts(self.output()), (len(CASES), 0))

    def test_missing_or_zero_results_do_not_pass(self):
        for output in (b"", self.output().split(b"\n", 1)[1], self.output().rsplit(b"\n", 1)[0]):
            with self.subTest(output_length=len(output)), self.assertRaises(TaskError):
                executed_counts(output)

    def test_duplicate_result_or_key_is_rejected(self):
        first = self.output().split(b"\n", 1)[0]
        for output in (
            first + b"\n" + self.output(),
            self.output().replace(b'"schema": 1', b'"schema": 1, "schema": 1', 1),
        ):
            with self.subTest(output_length=len(output)), self.assertRaises(TaskError):
                executed_counts(output)

    def test_unknown_content_or_oversize_is_rejected(self):
        for output in (
            self.output().replace(
                b'"passed": true', b'"native_error": "secret", "passed": true', 1
            ),
            b"x" * 65537,
        ):
            with self.subTest(output_length=len(output)), self.assertRaises(TaskError):
                executed_counts(output)

    def test_nonboolean_pass_and_incorrect_count_are_rejected(self):
        for output in (
            self.output().replace(b'"passed": true', b'"passed": 1', 1),
            self.output().replace(b'"failed": 0', b'"failed": 1'),
        ):
            with self.subTest(output_length=len(output)), self.assertRaises(TaskError):
                executed_counts(output)


def entry() -> int:
    arguments = sys.argv[1:]
    if arguments == ["--self-test"]:
        result = unittest.TextTestRunner().run(
            unittest.defaultTestLoader.loadTestsFromTestCase(OutputTests)
        )
        return 0 if result.wasSuccessful() else 1
    if arguments not in ([], ["--sanitizers"]):
        raise TaskError("keychain-tests-invalid-arguments")
    return run_task(sanitizers=bool(arguments))


if __name__ == "__main__":
    main(entry)
