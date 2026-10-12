#!/usr/bin/env -S python -I
# MISE description="Test guarded macOS Keychain calls with injected item operations only"
"""Build ordinary fixtures without opening, enrolling, or removing Keychain items."""

import json
import os
import re
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
        "fixed_item_rejects_slot_suffix_substitution",
        "public_retrieval_resolves_only_the_live_permit_scoped_item",
        "enrollment_records_exact_identity_and_reinspects_persisted_policy",
        "enrollment_collision_has_zero_effects",
        "enrollment_rechecks_scope_before_reservation_and_creation",
        "enrollment_never_reuses_a_reserved_slot_after_failure",
        "weakened_persisted_enrollment_keeps_receipt_for_exact_removal",
        "partial_install_record_failure_preserves_item_and_uncertainty",
        "uncertain_creation_keeps_the_original_pending_slot",
        "removal_retains_exact_item_through_namespace_replacement",
        "removal_rejects_unauthorized_or_replaced_receipt_items",
        "removal_record_failure_is_cleanup_unresolved",
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


def rust_composition(
    directory: Path, compiler: list[str], environment: dict[str, str], *, sanitizers: bool
) -> int:
    """Link the actual host crate to a native peer with no item-framework imports."""
    native = ROOT / "tests" / "server_native" / "macos"
    library = directory / "keychain-peer.dylib"
    command = [
        *compiler,
        "-dynamiclib",
        "-DST_KEYCHAIN_TESTING",
        "-DST_KEYCHAIN_RUST_TESTING",
        str(native / "keychain_items.c"),
        str(native / "keychain_items_tests.c"),
        "-framework",
        "Security",
        "-framework",
        "CoreFoundation",
        "-o",
        str(library),
    ]
    if sanitizers:
        command.extend(["-g", "-O1", "-fsanitize=address,undefined", "-fno-omit-frame-pointer"])
    result = subprocess.run(command, cwd=ROOT, env=environment, timeout=60)
    if result.returncode:
        return result.returncode
    symbols = subprocess.run(
        ["/usr/bin/nm", "-u", str(library)],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        timeout=10,
        check=True,
    )
    if b"_SecKeychain" in symbols.stdout or b"_SecItem" in symbols.stdout:
        raise TaskError("keychain-tests-composition-rejected")
    built = subprocess.run(
        [
            "cargo",
            "build",
            "--lib",
            "--package",
            "spaceterm-host",
            "--locked",
            "--message-format=json",
        ],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        timeout=240,
        check=True,
    )
    artifacts = []
    for line in built.stdout.splitlines():
        record = json.loads(line)
        if (
            record.get("reason") == "compiler-artifact"
            and record["target"]["name"] == "spaceterm_host"
        ):
            artifacts.extend(Path(name) for name in record["filenames"] if name.endswith(".rlib"))
    if len(artifacts) != 1:
        raise TaskError("keychain-tests-composition-rejected")
    binary = directory / "keychain-rust-tests"
    result = subprocess.run(
        [
            "rustc",
            "--edition=2024",
            "--test",
            "--extern",
            f"spaceterm_host={artifacts[0]}",
            "-L",
            f"dependency={artifacts[0].parent / 'deps'}",
            "-L",
            f"dependency={artifacts[0].parent}",
            str(
                ROOT
                / "crates"
                / "spaceterm-host"
                / "tests"
                / "native_management_composition"
                / "keychain_rust.rs"
            ),
            "-o",
            str(binary),
        ],
        cwd=ROOT,
        env=environment,
        timeout=60,
    )
    if result.returncode:
        return result.returncode
    peer_environment = {**environment, "SPACETERM_KEYCHAIN_TEST_PEER": str(library)}
    if sanitizers:
        runtime = subprocess.run(
            ["/usr/bin/xcrun", "clang", "-print-file-name=libclang_rt.asan_osx_dynamic.dylib"],
            cwd=ROOT,
            env=environment,
            capture_output=True,
            text=True,
            timeout=10,
            check=True,
        ).stdout.strip()
        if not Path(runtime).is_absolute() or not Path(runtime).is_file():
            raise TaskError("keychain-tests-composition-rejected")
        peer_environment["DYLD_INSERT_LIBRARIES"] = runtime
    result = subprocess.run(
        [str(binary), "--test-threads=1"],
        cwd=ROOT,
        env=peer_environment,
        capture_output=True,
        text=True,
        timeout=60,
    )
    counts = re.findall(
        r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored", result.stdout
    )
    if result.returncode != 0 or counts != [("1", "0", "0")]:
        raise TaskError("keychain-tests-rust-composition-failed")
    print("proof=synthetic scope=actual-host-rust-c tests=1 failed=0")
    return 0


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
            if result.returncode != 0 or failed != 0:
                return 1
            return rust_composition(Path(directory), compiler, environment, sanitizers=sanitizers)
        except subprocess.CalledProcessError:
            raise TaskError("keychain-tests-composition-rejected") from None
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
