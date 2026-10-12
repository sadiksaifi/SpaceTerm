"""Evidence validation is a gate, not a source of native authority."""

import copy
import io
import json
import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import TaskError
from spaceterm_tasks import native_evidence_schema as schema


def record(sequence=1, case="stopped-before-credential-and-retention"):
    return {
        "schema": 1,
        "scope": "native-deployment",
        "run_id": "1" * 32,
        "sequence": sequence,
        "job": "native-installation",
        "case": case,
        "host": {
            "platform": "macos",
            "architecture": "arm64",
            "os_version": [26, 6, 2],
            "kernel_version": [25, 6, 0],
            "libc_version": None,
            "supervisor": "launchd",
            "supervisor_version": None,
        },
        "revision": "2" * 40,
        "dirty": False,
        "artifact_sha256": "3" * 64,
        "context": "daemon-parented",
        "proof": "native",
        "result": "passed",
        "failure": None,
        "facts": {
            "kind": "lifecycle",
            "desired_state": "stopped",
            "credential_calls": 0,
            "installation_retention_calls": 0,
            "process_absent": True,
            "endpoint_absent": True,
        },
    }


def encoded(value):
    return json.dumps(value, separators=(",", ":")).encode() + b"\n"


class EvidenceSchemaTests(unittest.TestCase):
    def test_invalid_or_missing_records_cannot_become_passed(self):
        required = [schema.RequiredCase("native-installation", record()["case"], "daemon-parented")]
        self.assertEqual(schema.collect(io.BytesIO(encoded(record())), required).records, 1)
        for payload in (b"", encoded({}), encoded(record())[:-1]):
            with self.subTest(payload_length=len(payload)), self.assertRaises(TaskError):
                schema.collect(io.BytesIO(payload), required)
        preflight = record()
        preflight["proof"] = "preflight"
        with self.assertRaises(TaskError):
            schema.collect(io.BytesIO(encoded(preflight)), required)

    def test_passed_retrieval_requires_observed_retrieval_and_cleanup(self):
        value = record()
        value.update(
            job="macos-keychain",
            case="exact-startup-retrieval",
            facts={
                "kind": "credential",
                "policy": "unchecked",
                "interaction": "unchecked",
                "retrieved": False,
                "secret_wiped": False,
                "release_rechecked": False,
                "retriever_absent": False,
            },
        )
        required = [schema.RequiredCase(value["job"], value["case"], value["context"])]
        with self.assertRaisesRegex(TaskError, "^invalid-record$"):
            schema.collect(io.BytesIO(encoded(value)), required)

    def test_denied_retrieval_cannot_release_a_secret(self):
        value = record()
        value.update(
            job="macos-keychain",
            case="unprivileged-copy-denied",
            result="denied",
            failure="authorization-denied",
            facts={
                "kind": "credential",
                "policy": "accepted",
                "interaction": "disabled",
                "retrieved": True,
                "secret_wiped": True,
                "release_rechecked": True,
                "retriever_absent": True,
            },
        )
        required = [schema.RequiredCase(value["job"], value["case"], value["context"], "denied")]
        with self.assertRaisesRegex(TaskError, "^invalid-record$"):
            schema.collect(io.BytesIO(encoded(value)), required)

    def test_privilege_proof_requires_saved_ids_groups_descriptors_and_retirement(self):
        value = record()
        value.update(job="macos-launcher", case="privilege-and-inheritance-proof")
        required = [schema.RequiredCase(value["job"], value["case"], value["context"])]
        with self.assertRaisesRegex(TaskError, "^invalid-record$"):
            schema.collect(io.BytesIO(encoded(value)), required)

    def test_exact_retrieval_and_denial_observations_match_the_result(self):
        value = record()
        value.update(
            job="macos-keychain",
            case="exact-startup-retrieval",
            facts={
                "kind": "credential",
                "policy": "accepted",
                "interaction": "disabled",
                "retrieved": True,
                "secret_wiped": True,
                "release_rechecked": True,
                "retriever_absent": True,
            },
        )
        schema.collect(
            io.BytesIO(encoded(value)),
            [schema.RequiredCase(value["job"], value["case"], value["context"])],
        )
        for field in ("retrieved", "secret_wiped", "release_rechecked", "retriever_absent"):
            incomplete = copy.deepcopy(value)
            incomplete["facts"][field] = False
            with self.subTest(field=field), self.assertRaises(TaskError):
                schema.validate_record(incomplete)
        for case in schema.CREDENTIAL_DENIALS:
            value.update(
                case=case,
                result="denied",
                failure="credential-policy-rejected",
                facts={
                    "kind": "credential-denial",
                    "reason": case,
                    "attempt_observed": True,
                    "release_denied": True,
                    "retrieved": False,
                    "secret_wiped": True,
                    "retriever_absent": True,
                },
            )
            schema.validate_record(value)
            value["facts"]["retrieved"] = True
            with self.subTest(case=case), self.assertRaises(TaskError):
                schema.validate_record(value)

    def test_privilege_observations_cannot_omit_any_native_proof(self):
        value = record()
        fields = (
            "real_ids_verified effective_ids_verified saved_ids_verified supplementary_groups_cleared "
            "execution_bound hardened_runtime_verified descriptor_scope_verified secret_channels_closed "
            "custody_descriptors_absent retriever_absent root_process_absent root_listener_absent"
        ).split()
        value.update(
            job="macos-launcher",
            case="privilege-and-inheritance-proof",
            facts={
                "kind": "privilege",
                **dict.fromkeys(fields, True),
            },
        )
        required = [schema.RequiredCase(value["job"], value["case"], value["context"])]
        self.assertEqual(schema.collect(io.BytesIO(encoded(value)), required).passed, 1)
        for field in fields:
            incomplete = copy.deepcopy(value)
            incomplete["facts"][field] = False
            with self.subTest(field=field), self.assertRaises(TaskError):
                schema.collect(io.BytesIO(encoded(incomplete)), required)

    def test_required_profiles_have_closed_case_specific_observation_contracts(self):
        from spaceterm_tasks.server_native_macos import required_cases

        for profile in ("local", "disposable"):
            for case in required_cases(profile):
                with self.subTest(profile=profile, case=case.case):
                    self.assertIn((case.job, case.case), schema.CASE_FACTS)

    def test_required_startup_observes_approved_selection_and_actual_trigger(self):
        for job, case, trigger in (
            ("native-installation", "startup-never-selects-staged-installation", "none"),
            ("macos-management", "crash-restart", "crash"),
            ("macos-management", "explicit-start", "start"),
            ("macos-management", "approved-restart", "restart"),
        ):
            value = record()
            value.update(
                job=job,
                case=case,
                facts={
                    "kind": "startup",
                    "desired_state": "running",
                    "selection": "approved",
                    "trigger": trigger,
                    "readiness_authenticated": True,
                    "original_process_retired": True,
                    "endpoint_bound": True,
                },
            )
            schema.validate_record(value)
            for field, replacement in (
                ("selection", "staged"),
                ("readiness_authenticated", False),
                ("original_process_retired", False),
                ("endpoint_bound", False),
                ("desired_state", "stopped"),
            ):
                incomplete = copy.deepcopy(value)
                incomplete["facts"][field] = replacement
                with self.subTest(case=case, field=field), self.assertRaises(TaskError):
                    schema.validate_record(incomplete)

    def test_stop_requires_observed_started_write_settlement_before_bootout(self):
        value = record()
        value.update(
            job="macos-management",
            case="stop-drains-started-git-write-before-bootout",
            facts={
                "kind": "drain",
                "desired_state": "stopped",
                "started_writes": 1,
                "settled_writes": 1,
                "retriever_absent": True,
                "phase_order": [
                    "stopped-durable",
                    "admissions-closed",
                    "writes-settled",
                    "sessions-retired",
                    "process-absent",
                    "endpoint-absent",
                    "supervisor-removed",
                ],
            },
        )
        schema.validate_record(value)
        for field, replacement in (
            ("started_writes", 0),
            ("settled_writes", 0),
            ("retriever_absent", False),
            ("phase_order", list(reversed(value["facts"]["phase_order"]))),
        ):
            incomplete = copy.deepcopy(value)
            incomplete["facts"][field] = replacement
            with self.subTest(field=field), self.assertRaises(TaskError):
                schema.validate_record(incomplete)

    def test_authorization_result_requires_attempt_and_agrees_with_grant(self):
        for case, result, failure in (
            ("authorization-denied", "denied", "authorization-denied"),
            ("authorization-cancelled", "cancelled", "authorization-cancelled"),
            ("authorization-unavailable", "unavailable", "authorization-unavailable"),
            ("authorization-granted", "passed", None),
        ):
            value = record()
            value.update(
                job="macos-management",
                case=case,
                result=result,
                failure=failure,
                facts={
                    "kind": "authorization",
                    "operation": "start",
                    "attempt_observed": True,
                    "granted": result == "passed",
                    "caller_bound": result == "passed",
                    "helper_absent": True,
                },
            )
            schema.validate_record(value)
            for field, replacement in (
                ("attempt_observed", False),
                ("helper_absent", False),
                ("granted", result != "passed"),
            ):
                incomplete = copy.deepcopy(value)
                incomplete["facts"][field] = replacement
                with self.subTest(case=case, field=field), self.assertRaises(TaskError):
                    schema.validate_record(incomplete)

    def test_bootstrap_cannot_pass_without_protected_policy_and_root_effect_observation(self):
        value = record()
        fields = (
            "candidate_manifest_verified candidate_signatures_verified protected_admin_digest_verified "
            "protected_admin_signature_verified protected_admin_policy_verified verified_before_root_exec "
            "receipt_durable_before_effects root_execution_observed fresh_copy collision_check_complete"
        ).split()
        value.update(
            job="macos-evidence",
            case="bootstrap-authenticates-protected-admin-before-root-exec",
            facts={
                "kind": "bootstrap",
                **dict.fromkeys(fields, True),
            },
        )
        schema.validate_record(value)
        for field in fields:
            incomplete = copy.deepcopy(value)
            incomplete["facts"][field] = False
            with self.subTest(field=field), self.assertRaises(TaskError):
                schema.validate_record(incomplete)

    def test_unmodeled_case_cannot_credit_a_native_pass(self):
        value = record(case="boot-before-login")
        with self.assertRaises(TaskError):
            schema.validate_record(value)
        value["result"] = "unrun"
        schema.validate_record(value)

    def test_record_rejects_unknown_fields_and_sensitive_values(self):
        for mutation in (
            lambda r: r.update(path="/private/sensitive"),
            lambda r: r["facts"].update(native_error="secret"),
            lambda r: r["host"].update(account="someone"),
            lambda r: r.update(case="/private/path"),
            lambda r: r.update(failure="native-error-with-secret"),
            lambda r: r["facts"].update(desired_state="HOME=/private/sensitive"),
        ):
            value = record()
            mutation(value)
            with self.assertRaisesRegex(TaskError, "^invalid-record$"):
                schema.validate_record(value)

    def test_duplicates_nonfinite_values_and_noncanonical_types_reject(self):
        good = encoded(record())
        for data in (
            good.replace(b'"schema":1', b'"schema":1,"schema":1'),
            good.replace(b'"sequence":1', b'"sequence":NaN'),
            good.replace(b'"sequence":1', b'"sequence":true'),
            good.replace(b'"sequence":1', b'"sequence":1.0'),
            good.replace(b'"sequence":1', b'"sequence":-1'),
            b"\xff\n",
            b"[]\n",
        ):
            with self.subTest(data_length=len(data)), self.assertRaises(TaskError):
                schema.collect(io.BytesIO(data), [])

    def test_sequence_gaps_duplicates_and_run_substitution_reject(self):
        for second in (record(1), record(3), record(2)):
            with self.subTest(sequence=second["sequence"]), self.assertRaises(TaskError):
                schema.collect(io.BytesIO(encoded(record()) + encoded(second)), [])
        for field, value in (("run_id", "4" * 32), ("artifact_sha256", "5" * 64)):
            second = record(2, "startup-never-selects-staged-installation")
            second[field] = value
            with self.assertRaises(TaskError):
                schema.collect(io.BytesIO(encoded(record()) + encoded(second)), [])

    def test_collector_bounds_reads_and_rejects_oversized_records(self):
        class BoundedStream(io.BytesIO):
            def readline(self, size=-1):
                if not 0 < size <= 8193:
                    raise AssertionError("unbounded read")
                return super().readline(size)

        with self.assertRaisesRegex(TaskError, "^limit-exceeded$"):
            schema.collect(BoundedStream(b"x" * 8193 + b"\n"), [])
        summary = schema.collect(BoundedStream(encoded(record())), [])
        self.assertEqual(summary.records, 1)

    def test_native_pass_requires_effect_and_cleanup_observation(self):
        value = record()
        value.update(
            job="macos-capabilities",
            case="microphone",
            context="gui-parented",
            facts={
                "kind": "capability",
                "capability": 3,
                "preflight": "passed",
                "effect": "unrun",
                "responsible_process": "unknown",
                "cleanup": "complete",
            },
        )
        with self.assertRaises(TaskError):
            schema.validate_record(value)
        value["facts"].update(effect="passed", responsible_process="probe")
        schema.validate_record(value)
        value["facts"]["cleanup"] = "unresolved"
        with self.assertRaises(TaskError):
            schema.validate_record(value)

    def test_gui_and_daemon_results_are_never_coalesced(self):
        gui = record()
        gui["context"] = "gui-parented"
        daemon = record(2)
        required = [
            schema.RequiredCase("native-installation", gui["case"], context)
            for context in ("gui-parented", "daemon-parented")
        ]
        self.assertEqual(
            schema.collect(io.BytesIO(encoded(gui) + encoded(daemon)), required).records, 2
        )
        with self.assertRaises(TaskError):
            schema.collect(io.BytesIO(encoded(gui)), required)

    def test_cleanup_unresolved_and_unrun_are_retained_not_completion(self):
        required = [schema.RequiredCase("native-installation", record()["case"], "daemon-parented")]
        for result, failure in (
            ("unrun", None),
            ("cleanup-unresolved", "cleanup-unresolved"),
            ("indeterminate", "indeterminate"),
        ):
            value = record()
            value.update(result=result, failure=failure)
            schema.validate_record(value)
            with self.assertRaisesRegex(TaskError, "^outcome-pending$"):
                schema.collect(io.BytesIO(encoded(value)), required)

    def test_output_is_new_private_and_contains_only_validated_records(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "evidence.jsonl"
            with schema.EvidenceOutput(path) as output:
                output.append(encoded(record()))
                self.assertEqual(output.finish([]).records, 1)
                with self.assertRaises(TaskError):
                    output.append(encoded({"native_error": "sensitive"}))
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(json.loads(path.read_bytes()), record())
            with self.assertRaisesRegex(TaskError, "^unsafe-native-state$"):
                with schema.EvidenceOutput(path):
                    self.fail("overwrote existing output")
            alias = Path(directory) / "alias.jsonl"
            alias.symlink_to(path)
            with self.assertRaises(TaskError):
                with schema.EvidenceOutput(alias):
                    self.fail("followed output symlink")
            self.assertEqual(json.loads(path.read_bytes()), record())

    def test_output_failure_is_typed_and_never_claims_completion(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "evidence.jsonl"
            output = schema.EvidenceOutput(path)
            with output:
                output.append(encoded(record()))
            with self.assertRaisesRegex(TaskError, "^io-unavailable$"):
                output.finish([])

    def test_validation_does_not_modify_input(self):
        value = record()
        original = copy.deepcopy(value)
        schema.validate_record(value)
        self.assertEqual(value, original)


if __name__ == "__main__":
    unittest.main()
