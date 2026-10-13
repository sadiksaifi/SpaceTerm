"""Unprivileged package and recovery tests. No signing import or host mutation."""

import base64
import copy
import hashlib
import io
import json
import os
import signal
import subprocess
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from spaceterm_tasks import TaskError
from spaceterm_tasks import server_native_macos as native
from spaceterm_tasks.native_evidence_schema import Collector, collect

from tests.test_native_evidence_schema import encoded, record


def fixtures(root):
    files = []
    for name in sorted(native.CODE_IDENTIFIERS):
        path = root / native.code_file(name)
        path.parent.mkdir(parents=True, exist_ok=True)
        data = b"\xcf\xfa\xed\xfe" + name.encode()
        path.write_bytes(data)
        path.chmod(0o555)
        files.append(
            {
                "name": native.code_file(name),
                "size": len(data),
                "sha256": hashlib.sha256(data).hexdigest(),
                "mode": 0o555,
            }
        )
    files.sort(key=lambda item: item["name"])
    manifest = {
        "schema": 1,
        "scope": "native-evidence-520",
        "run_id": "1" * 32,
        "revision": "2" * 40,
        "dirty": False,
        "files": files,
        "code": [
            {"name": name, "identifier": identifier}
            for name, identifier in sorted(native.CODE_IDENTIFIERS.items())
        ],
    }
    (root / "fixture-manifest.json").write_bytes(native.canonical_json(manifest))
    return manifest


class SignedCode:
    """Control native signature observations, not package validation or copying."""

    def __init__(self, accepted=True):
        self.accepted = accepted
        self.verified = []

    def verify(self, path, identifier):
        self.verified.append((path, identifier))
        if not self.accepted:
            raise TaskError("artifact-rejected")


class FixtureOperations:
    def __init__(self, collision=False, fail_at=None, cleanup_unresolved=False):
        self.collision = collision
        self.fail_at = fail_at
        self.cleanup_unresolved = cleanup_unresolved
        self.effects = []
        self.removed = []
        self.store: native.ReceiptStore | None = None
        self.current = {}

    def inspect_collisions(self, receipt):
        if self.collision:
            raise TaskError("unsafe-native-state")

    def install(self, resource, receipt, record_identity):
        assert self.store is not None
        durable = self.store.read()
        if durable["objects"][resource]["state"] != "pending":
            raise AssertionError("effect before durable intent")
        for previous in native.RESOURCES[: native.RESOURCES.index(resource)]:
            if durable["objects"][previous]["state"] != "installed":
                raise AssertionError("next effect before completed identity publication")
        self.effects.append(resource)
        identities = effect_identities(resource, receipt)
        self.current[resource] = copy.deepcopy(identities)
        for identity in identities:
            record_identity(identity)
        durable = self.store.read()
        if [
            binding["identity"] for binding in durable["objects"][resource]["bindings"]
        ] != identities:
            raise AssertionError("native identity not durably recorded")
        if resource == self.fail_at:
            raise TaskError("io-unavailable")
        return identities

    def retire(self, receipt):
        scope_verified = all(
            binding["identity"] in self.current.get("service", [])
            for binding in receipt["objects"]["service"]["bindings"]
            if binding["identity"]["kind"] == "process"
        )
        return native.CleanupObservation(
            scope_verified=scope_verified,
            process_absent=not self.cleanup_unresolved,
            retriever_absent=True,
            sessions_absent=True,
            endpoint_absent=True,
        )

    def retain_cleanup_bindings(self, resource, receipt):
        return copy.deepcopy(self.current.get(resource, []))

    def remove(self, resource, receipt):
        expected = [binding["identity"] for binding in receipt["objects"][resource]["bindings"]]
        if self.current.get(resource) != expected:
            raise TaskError("cleanup-unresolved")
        self.removed.append(resource)
        del self.current[resource]


def effect_identities(resource, receipt):
    if resource in ("fixtures", "registration"):
        return [
            {
                "kind": "filesystem",
                "name": "evidence-admin"
                if resource == "fixtures"
                else receipt["registration_name"],
                "file_type": "file",
                "volume_uuid": "a" * 32,
                "device": 1,
                "inode": 10 if resource == "fixtures" else 20,
                "generation": 1,
                "birth_seconds": 100,
                "birth_nanoseconds": 7,
                "artifact_sha256": receipt["artifacts"]["evidence-admin"]
                if resource == "fixtures"
                else receipt["registration_sha256"],
            }
        ]
    if resource == "credential":
        return [
            {
                "kind": "credential-item",
                "keychain": "system",
                "persistent_reference": base64.b64encode(b"native item instance one").decode(),
                "credential_binding_sha256": receipt["credential_binding_sha256"],
            }
        ]
    return [
        {"kind": "service", "label": receipt["service_label"], "native_instance_id": "b" * 64},
        {
            "kind": "process",
            "role": "server",
            "pid": 100,
            "start_seconds": 200,
            "start_microseconds": 10,
            "boot_id": "c" * 32,
            "executable_sha256": receipt["artifacts"]["fixture-server"],
        },
    ]


def receipt():
    return native.new_receipt(
        run_id="1" * 32,
        account_uid=501,
        primary_gid=20,
        account_uuid="4" * 32,
        server_id="6" * 32,
        credential_slot="7" * 32,
        credential_binding_sha256="8" * 64,
        registration_sha256="9" * 64,
        artifact_sha256="3" * 64,
        artifacts={name: "5" * 64 for name in native.CODE_IDENTIFIERS},
    )


class NativeMacOSEvidenceTests(unittest.TestCase):
    def test_local_shell_pass_requires_normalized_observation_in_each_context(self):
        required = [
            case for case in native.required_cases("local") if case.case == "shell-environment"
        ]
        observations = []
        for context in ("gui-parented", "daemon-parented"):
            capability = record()
            capability.update(
                job="macos-capabilities",
                case="shell-environment",
                context=context,
                facts={
                    "kind": "capability",
                    "capability": 13,
                    "preflight": "passed",
                    "effect": "passed",
                    "responsible_process": "probe",
                    "cleanup": "not-needed",
                },
            )
            comparison = record()
            comparison.update(
                job="macos-capabilities",
                case="normalized-shell-comparison",
                context=context,
                facts={
                    "kind": "environment-comparison",
                    "reference": "sanitized-launch-v1",
                    "path_difference": "components-changed",
                    "locale_difference": "values-changed",
                    "login": True,
                    "interactive": True,
                    "startup_order_verified": True,
                },
            )
            observations.extend((capability, comparison))

        def collected(values):
            payload = b"".join(
                encoded(dict(value, sequence=sequence)) for sequence, value in enumerate(values, 1)
            )
            return collect(io.BytesIO(payload), required)

        self.assertEqual(collected(observations).passed, 4)
        for context in ("gui-parented", "daemon-parented"):
            missing = [
                value
                for value in observations
                if (value["case"], value["context"]) != ("normalized-shell-comparison", context)
            ]
            with (
                self.subTest(context=context),
                self.assertRaisesRegex(TaskError, "^invalid-record$"),
            ):
                collected(missing)
            unavailable = copy.deepcopy(observations)
            comparison = next(
                value
                for value in unavailable
                if value["case"] == "normalized-shell-comparison" and value["context"] == context
            )
            comparison.update(result="unavailable", failure="native-unavailable")
            comparison["facts"]["startup_order_verified"] = False
            with (
                self.subTest(unavailable_context=context),
                self.assertRaisesRegex(TaskError, "^outcome-pending$"),
            ):
                collected(unavailable)

        # The probe can honestly report that no comparison could run. No missing
        # normalized observation may be credited as a passed shell comparison.
        unavailable = [
            copy.deepcopy(value) for value in observations if value["case"] == "shell-environment"
        ]
        for capability in unavailable:
            capability.update(result="unavailable", failure="native-unavailable")
            capability["facts"].update(
                preflight="unavailable", effect="unavailable", responsible_process="unknown"
            )
        self.assertEqual(collected(unavailable).passed, 0)

    def test_invalid_manifest_never_produces_runnable_runbook(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.mkdir()
            manifest = fixtures(source)
            for mutate in (
                lambda m: m.update(unknown="/private/sensitive"),
                lambda m: m["files"][0].update(name="../escape"),
                lambda m: m["files"][0].update(name="\ud800"),
                lambda m: m["files"][0].update(sha256="0" * 64),
            ):
                malformed = json.loads(json.dumps(manifest))
                mutate(malformed)
                (source / "fixture-manifest.json").write_bytes(native.canonical_json(malformed))
                destination = root / "candidate"
                with self.assertRaises(TaskError):
                    native.prepare(source, destination, SignedCode())
                self.assertFalse((destination / "runbook-local.sh").exists())

    def test_unsigned_preparation_emits_only_nonrunnable_preflight(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.mkdir()
            fixtures(source)
            prepared = native.prepare(source, root / "candidate", SignedCode(False))
            self.assertFalse(prepared.runnable)
            self.assertFalse((root / "candidate/runbook-local.sh").exists())
            self.assertFalse((root / "candidate/runbook-disposable.sh").exists())
            self.assertEqual(prepared.failure, "artifact-rejected")

    def test_bootstrap_authenticates_protected_admin_before_root_exec(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.mkdir()
            fixtures(source)
            verifier = SignedCode()
            prepared = native.prepare(source, root / "candidate", verifier)
            self.assertTrue(prepared.runnable)
            self.assertTrue(
                all(path.is_relative_to(root / "candidate") for path, _ in verifier.verified)
            )
            local = (root / "candidate/runbook-local.sh").read_text()
            self.assertLess(
                local.index('verify_admin "$ROOT/evidence-admin"'), local.index("admin install")
            )
            self.assertIn(prepared.artifact_sha256, local)
            self.assertNotIn('sudo "$STAGE/', local)
            self.assertNotIn("rm -r", local)
            self.assertIn("/usr/bin/env -i", local)
            self.assertIn("admin uninstall", local)

    def test_policy_observations_reject_replacement_restored_before_final_hash(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.mkdir()
            fixtures(source)
            substituted = []

            def native_tool(arguments, timeout=10.0):
                path = Path(arguments[-1])
                if arguments[0] == "/usr/bin/codesign" and "--verbose=4" in arguments:
                    if path.name == "evidence-admin":
                        alternate = path.with_name("alternate")
                        retained = path.with_name("original")
                        alternate.write_bytes(b"another signed variant with hardened runtime")
                        path.rename(retained)
                        alternate.rename(path)
                        # The policy tool sees the alternate. Restore the manifest bytes.
                        path.rename(alternate)
                        retained.rename(path)
                        alternate.unlink()
                        substituted.append(path)
                    return b"", b"CodeDirectory flags=0x10000(runtime)\n"
                if arguments[0] == "/usr/bin/otool" and "-L" in arguments:
                    return (
                        b"fixture:\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0)\n",
                        b"",
                    )
                if arguments[0] == "/usr/bin/lipo":
                    return b"arm64\n", b""
                return b"", b""

            with (
                patch("platform.system", return_value="Darwin"),
                patch("platform.machine", return_value="arm64"),
                patch("platform.mac_ver", return_value=("26.6.2", (), "")),
                patch.object(native, "_tool", side_effect=native_tool),
            ):
                prepared = native.prepare(source, root / "candidate", native.NativeCodeVerifier())
            self.assertTrue(substituted)
            self.assertFalse(prepared.runnable)
            self.assertEqual(prepared.failure, "artifact-rejected")
            self.assertFalse((root / "candidate/runbook-local.sh").exists())

    def test_policy_observations_reject_parent_directory_swapped_and_restored(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.mkdir()
            fixtures(source)
            candidate = root / "candidate"

            class DirectoryReplacement(SignedCode):
                def verify(self, path, identifier):
                    super().verify(path, identifier)
                    candidate.rename(root / "retained-directory")
                    candidate.mkdir()
                    candidate.rename(root / "replacement-directory")
                    (root / "retained-directory").rename(candidate)
                    (root / "replacement-directory").rmdir()

            prepared = native.prepare(source, candidate, DirectoryReplacement())
            self.assertFalse(prepared.runnable)
            self.assertEqual(prepared.failure, "artifact-rejected")
            self.assertFalse((candidate / "runbook-local.sh").exists())

    def test_protected_admin_rechecks_every_code_policy_before_root_exec(self):
        with tempfile.TemporaryDirectory() as directory:
            sequence = native.render_runbook(fixtures(Path(directory)), "3" * 64)
        functions = (
            "fail() {" + sequence.split("\nfail() {", 1)[1].split("\ntrap finish EXIT", 1)[0]
        )
        functions = functions.replace("/usr/bin/codesign", "code_tool")
        admin_digest = hashlib.sha256(b"\xcf\xfa\xed\xfe" + b"evidence-admin").hexdigest()
        for defect in (
            "none",
            "runtime",
            "entitlements",
            "dependencies",
            "load-commands",
            "architecture",
        ):
            with self.subTest(defect=defect):
                program = (
                    "set -eu\n"
                    + functions
                    + f"\nDEFECT={defect}\nADMIN_DIGEST={admin_digest}\n"
                    + """
code_tool() { return 0; }
hash_matches() { return 0; }
verified_tool() {
  case "$*" in
    *shasum*) printf '%s fixture\\n' "$ADMIN_DIGEST" ;;
    *--verbose=4*) if test "$DEFECT" = runtime; then printf 'CodeDirectory flags=0x0\n'; else printf 'CodeDirectory flags=0x10000(runtime)\n'; fi ;;
    *--entitlements*) if test "$DEFECT" = entitlements; then printf '<plist><dict><key>com.apple.security.get-task-allow</key><true/></dict></plist>'; fi ;;
    *' -L '*) if test "$DEFECT" = dependencies; then printf 'fixture:\n /unprotected/library (version 1)\n'; else printf 'fixture:\n /usr/lib/libSystem.B.dylib (version 1)\n'; fi ;;
    *' -l '*) if test "$DEFECT" = load-commands; then printf 'LC_RPATH\n'; fi ;;
    *-archs*) if test "$DEFECT" = architecture; then printf 'x86_64\n'; else printf 'arm64\n'; fi ;;
  esac
}
if verify_admin fixture; then exit 0; else exit 1; fi
"""
                )
                result = subprocess.run(
                    ["/bin/sh"],
                    input=program.encode(),
                    capture_output=True,
                    env=native.native_environment(),
                    cwd="/",
                    timeout=5,
                )
                self.assertEqual(result.returncode, 0 if defect == "none" else 1, result.stderr)
        self.assertLess(
            sequence.index('verify_admin "$ROOT/evidence-admin" || fail'),
            sequence.index("admin install"),
        )

    def test_runbook_collision_never_uninstalls_competing_root(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = fixtures(Path(directory))
            sequence = native.render_runbook(manifest, "3" * 64)
        # Execute the generated bootstrap and cleanup logic. Only the external
        # mkdir and admin boundaries are replaced; no native operation is run.
        functions = (
            "fail() {" + sequence.split("\nfail() {", 1)[1].split("\ntrap finish EXIT", 1)[0]
        )
        bootstrap = sequence.split("\n/usr/bin/sudo -v\n", 1)[1].split("# Bootstrap metadata", 1)[0]
        bootstrap = bootstrap.replace("/usr/bin/sudo", "native_sudo")
        program = (
            "set -eu\nBOOTSTRAP_STARTED=0\nBOOTSTRAP_PHASE=0\nCLEANUP_ACTIVE=0\nFIRST_SIGNAL=0\nROOT=unused\n"
            + functions
            + '\nadmin() { /usr/bin/printf "uninstall\\n"; }\nnative_sudo() { return 1; }\ntrap finish EXIT\n'
            + bootstrap
        )
        result = subprocess.run(
            ["/bin/sh"],
            input=program.encode(),
            capture_output=True,
            env=native.native_environment(),
            cwd="/",
            timeout=5,
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, b"")
        self.assertEqual(result.stderr, b"failure=unsafe-native-state\n")

    def test_runbook_cleanup_signal_keeps_uncertainty_precedence(self):
        with tempfile.TemporaryDirectory() as directory:
            sequence = native.render_runbook(fixtures(Path(directory)), "3" * 64)
        functions = (
            "fail() {" + sequence.split("\nfail() {", 1)[1].split("\ntrap finish EXIT", 1)[0]
        )
        for cleanup_status, expected in ((0, 143), (1, 75)):
            with self.subTest(cleanup_status=cleanup_status):
                program = (
                    "set -eu\nBOOTSTRAP_STARTED=1\nBOOTSTRAP_PHASE=2\nCLEANUP_ACTIVE=0\nFIRST_SIGNAL=0\n"
                    + functions
                    + f"\nadmin() {{ cancel 15; cancel 1; return {cleanup_status}; }}\ntrap finish EXIT\nexit 0\n"
                )
                result = subprocess.run(
                    ["/bin/sh"],
                    input=program.encode(),
                    capture_output=True,
                    env=native.native_environment(),
                    cwd="/",
                    timeout=5,
                )
                self.assertEqual(result.returncode, expected)
                self.assertEqual(result.stdout, b"")
                self.assertEqual(
                    result.stderr, b"failure=cleanup-unresolved\n" if cleanup_status else b""
                )

    def test_reading_a_fixture_does_not_reject_kernel_access_time_updates(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "fixture").write_bytes(b"fixture bytes")
            original = os.fstat
            calls = 0

            def access_time_update(descriptor):
                nonlocal calls
                calls += 1
                info = original(descriptor)
                fields = {key: getattr(info, key) for key in dir(info) if key.startswith("st_")}
                if calls > 1:
                    fields["st_atime"] += 1
                    fields["st_atime_ns"] += 1000000000
                return SimpleNamespace(**fields)

            with patch("os.fstat", side_effect=access_time_update):
                self.assertEqual(native.read_regular(root, "fixture", 1024), b"fixture bytes")

    def test_package_rejects_links_extra_files_and_copy_substitution(self):
        for kind in ("symlink", "hardlink", "extra", "tampered"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                source = root / "source"
                source.mkdir()
                fixtures(source)
                target = source / "evidence-admin"
                if kind == "symlink":
                    target.unlink()
                    target.symlink_to("evidence-client")
                elif kind == "hardlink":
                    os.link(target, source / "alias")
                elif kind == "extra":
                    (source / "json.py").write_text("raise RuntimeError('untrusted')")
                else:
                    target.chmod(0o755)
                    target.write_bytes(b"replacement")
                with self.assertRaises(TaskError):
                    native.prepare(source, root / "candidate", SignedCode())
                self.assertFalse((root / "candidate/runbook-local.sh").exists())

    def test_collision_refuses_before_first_install_effect(self):
        with tempfile.TemporaryDirectory() as directory:
            store = native.ReceiptStore(Path(directory))
            operations = FixtureOperations(collision=True)
            operations.store = store
            outcome = native.orchestrate(receipt(), store, operations, lambda: None)
            self.assertEqual(outcome.failure, "unsafe-native-state")
            self.assertEqual(operations.effects, [])
            self.assertEqual(operations.removed, [])
            self.assertFalse(store.exists())

    def test_receipt_is_durable_before_first_effect(self):
        with tempfile.TemporaryDirectory() as directory:
            store = native.ReceiptStore(Path(directory))
            operations = FixtureOperations()
            operations.store = store
            outcome = native.orchestrate(receipt(), store, operations, lambda: None)
            self.assertEqual(outcome.failure, None)
            self.assertEqual(operations.effects, list(native.RESOURCES))
            self.assertEqual(store.read()["state"], "cleaned")

    def test_partial_install_cleanup_removes_only_recorded_resources(self):
        with tempfile.TemporaryDirectory() as directory:
            store = native.ReceiptStore(Path(directory))
            operations = FixtureOperations(fail_at="registration")
            operations.store = store
            outcome = native.orchestrate(receipt(), store, operations, lambda: None)
            self.assertEqual(outcome.failure, "io-unavailable")
            self.assertEqual(set(operations.removed), set(operations.effects))
            self.assertNotIn("credential", operations.removed)
            self.assertEqual(store.read()["state"], "cleaned")

    def test_install_returns_exact_identities_before_the_next_effect(self):
        with tempfile.TemporaryDirectory() as directory:
            store = native.ReceiptStore(Path(directory))
            operations = FixtureOperations()
            operations.store = store
            installed = []

            def inspect_installed_receipt():
                installed.append(store.read())

            outcome = native.orchestrate(receipt(), store, operations, inspect_installed_receipt)
            self.assertIsNone(outcome.failure)
            self.assertIsInstance(installed[0]["objects"]["fixtures"], dict)
            self.assertTrue(installed[0]["objects"]["fixtures"]["bindings"])

    def test_crash_before_identity_publication_never_deletes_by_resource_name(self):
        with tempfile.TemporaryDirectory() as directory:
            store = native.ReceiptStore(Path(directory))
            operations = FixtureOperations(fail_at="registration")
            operations.store = store
            original = operations.install

            def crash_before_binding(resource, *arguments):
                if resource == "registration":
                    raise TaskError("io-unavailable")
                return original(resource, *arguments)

            with patch.object(operations, "install", side_effect=crash_before_binding):
                outcome = native.orchestrate(receipt(), store, operations, lambda: None)
            self.assertEqual(outcome.failure, "cleanup-unresolved")
            self.assertNotIn("registration", operations.removed)
            self.assertEqual(store.read()["state"], "cleanup-unresolved")

    def test_cleanup_refuses_recreated_files_items_and_recycled_processes(self):
        for resource, field, replacement in (
            ("fixtures", "inode", 99),
            (
                "credential",
                "persistent_reference",
                base64.b64encode(b"replacement native item").decode(),
            ),
            ("service", "start_microseconds", 11),
        ):
            with self.subTest(resource=resource), tempfile.TemporaryDirectory() as directory:
                store = native.ReceiptStore(Path(directory))
                operations = FixtureOperations()
                operations.store = store

                def replace_native_object():
                    index = 1 if resource == "service" else 0
                    operations.current[resource][index][field] = replacement

                outcome = native.orchestrate(receipt(), store, operations, replace_native_object)
                self.assertEqual(outcome.failure, "cleanup-unresolved")
                self.assertNotIn(resource, operations.removed)
                self.assertEqual(store.read()["state"], "cleanup-unresolved")
                self.assertIn(resource, operations.current)
                with self.assertRaisesRegex(TaskError, "^cleanup-unresolved$"):
                    native.cleanup_receipt(store, operations)
                self.assertIn(resource, operations.current)

    def test_cleanup_preserves_same_content_file_recreated_on_a_new_inode(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installed = root / "evidence-admin"
            content = b"same authenticated bytes"
            installed.write_bytes(content)
            value = receipt()
            value["artifacts"]["evidence-admin"] = hashlib.sha256(content).hexdigest()
            original_identities = effect_identities

            def identities(resource, receipt):
                result = original_identities(resource, receipt)
                if resource == "fixtures":
                    info = installed.stat()
                    result[0].update(
                        inode=info.st_ino,
                        device=info.st_dev,
                        birth_seconds=info.st_ctime_ns // 1000000000,
                        birth_nanoseconds=info.st_ctime_ns % 1000000000,
                    )
                return result

            store = native.ReceiptStore(root)
            operations = FixtureOperations()
            operations.store = store
            original_observe = operations.retain_cleanup_bindings

            def observe(resource, receipt):
                return (
                    identities(resource, receipt)
                    if resource == "fixtures"
                    else original_observe(resource, receipt)
                )

            def recreate_file():
                # Retain the original inode so the filesystem cannot reuse it.
                installed.rename(root / "original-inode")
                installed.write_bytes(content)

            with (
                patch("tests.test_server_native_macos.effect_identities", side_effect=identities),
                patch.object(operations, "retain_cleanup_bindings", side_effect=observe),
            ):
                outcome = native.orchestrate(value, store, operations, recreate_file)
            self.assertEqual(outcome.failure, "cleanup-unresolved")
            self.assertNotIn("fixtures", operations.removed)
            self.assertEqual(installed.read_bytes(), content)
            self.assertEqual(store.read()["state"], "cleanup-unresolved")

    def test_pending_identity_survives_installation_crash_and_recovery(self):
        with tempfile.TemporaryDirectory() as directory:
            store = native.ReceiptStore(Path(directory))
            operations = FixtureOperations(cleanup_unresolved=True, fail_at="credential")
            operations.store = store
            outcome = native.orchestrate(receipt(), store, operations, lambda: None)
            self.assertEqual(outcome.failure, "cleanup-unresolved")
            retained = store.read()["objects"]["credential"]
            self.assertEqual(retained["state"], "pending")
            self.assertEqual(retained["bindings"][0]["state"], "pending")
            self.assertEqual(
                retained["bindings"][0]["identity"], operations.current["credential"][0]
            )
            operations.cleanup_unresolved = False
            native.cleanup_receipt(store, operations)
            self.assertEqual(store.read()["state"], "cleaned")
            self.assertEqual(set(operations.removed), {"fixtures", "registration", "credential"})

    def test_receipt_rejects_missing_unknown_and_legacy_identity_records(self):
        value = receipt()
        for mutate in (
            lambda r: r.update(schema=1),
            lambda r: r["objects"]["fixtures"].update(state="installed"),
            lambda r: r["objects"]["fixtures"].update(path="/arbitrary"),
        ):
            malformed = copy.deepcopy(value)
            mutate(malformed)
            with self.assertRaises(TaskError):
                native.validate_receipt(malformed)

    def test_signal_cancellation_preserves_first_signal_and_cleanup_precedence(self):
        for uncertain in (False, True):
            with self.subTest(uncertain=uncertain), tempfile.TemporaryDirectory() as directory:
                store = native.ReceiptStore(Path(directory))
                operations = FixtureOperations(cleanup_unresolved=uncertain)
                operations.store = store

                def interrupted():
                    raise SystemExit(128 + signal.SIGTERM)

                outcome = native.orchestrate(receipt(), store, operations, interrupted)
                self.assertEqual(outcome.signal, signal.SIGTERM)
                self.assertEqual(outcome.exit_code, 75 if uncertain else 143)
                self.assertEqual(
                    outcome.failure,
                    "cleanup-unresolved" if uncertain else "authorization-cancelled",
                )
                if uncertain:
                    self.assertEqual(operations.removed, [])

    def test_failed_output_channel_does_not_skip_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            store = native.ReceiptStore(Path(directory))
            operations = FixtureOperations()
            operations.store = store

            def broken_output():
                collector = Collector()
                closed = io.BytesIO()
                closed.close()
                collector.append(encoded(record()), closed)

            outcome = native.orchestrate(receipt(), store, operations, broken_output)
            self.assertEqual(outcome.failure, "io-unavailable")
            self.assertEqual(set(operations.removed), set(native.RESOURCES))
            self.assertEqual(store.read()["state"], "cleaned")

    def test_cleanup_uncertainty_retains_receipt_and_protected_fixture(self):
        with tempfile.TemporaryDirectory() as directory:
            store = native.ReceiptStore(Path(directory))
            operations = FixtureOperations(cleanup_unresolved=True)
            operations.store = store
            outcome = native.orchestrate(receipt(), store, operations, lambda: None)
            self.assertEqual(outcome.failure, "cleanup-unresolved")
            self.assertTrue(store.exists())
            self.assertEqual(store.read()["state"], "cleanup-unresolved")
            self.assertEqual(operations.removed, [])

    def test_sync_uncertainty_never_becomes_durable_after_retry(self):
        with tempfile.TemporaryDirectory() as directory:
            store = native.ReceiptStore(Path(directory))
            operations = FixtureOperations()
            operations.store = store
            with patch("os.fsync", side_effect=OSError("private sensitive message")):
                outcome = native.orchestrate(receipt(), store, operations, lambda: None)
            self.assertEqual(outcome.failure, "indeterminate")
            self.assertEqual(operations.effects, [])
            with self.assertRaisesRegex(TaskError, "^indeterminate$"):
                store.publish(receipt())

    def test_recreated_store_cannot_clean_an_uncertain_identity_publication(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            store = native.ReceiptStore(root)
            operations = FixtureOperations()
            operations.store = store
            store.publish(receipt())
            changed = receipt()
            changed["state"] = "installing"
            changed["objects"]["fixtures"]["state"] = "pending"
            # The rename happened, but its directory synchronization failed.
            real_sync = os.fsync

            def fail_directory_sync(descriptor):
                import stat

                if stat.S_ISDIR(os.fstat(descriptor).st_mode):
                    raise OSError("private native detail")
                real_sync(descriptor)

            with patch("os.fsync", side_effect=fail_directory_sync):
                with self.assertRaisesRegex(TaskError, "^indeterminate$"):
                    store.publish(changed)
            recreated = native.ReceiptStore(root)
            with self.assertRaisesRegex(TaskError, "^indeterminate$"):
                native.cleanup_receipt(recreated, operations)
            self.assertEqual(operations.removed, [])

    def test_unrecorded_pending_process_scope_never_reaches_automatic_retirement(self):
        for state, service_only in (("pending", False), ("pending", True), ("installed", True)):
            with (
                self.subTest(state=state, service_only=service_only),
                tempfile.TemporaryDirectory() as directory,
            ):
                store = native.ReceiptStore(Path(directory))
                value = receipt()
                value["state"] = "installing"
                value["objects"]["service"]["state"] = state
                if service_only:
                    value["objects"]["service"]["bindings"] = [
                        {
                            "state": "pending" if state == "pending" else "completed",
                            "identity": effect_identities("service", value)[0],
                        }
                    ]
                store.publish(value)
                operations = FixtureOperations()
                operations.store = store
                with patch.object(operations, "retire", wraps=operations.retire) as retire:
                    with self.assertRaisesRegex(TaskError, "^cleanup-unresolved$"):
                        native.cleanup_receipt(store, operations)
                    retire.assert_not_called()
                self.assertEqual(store.read()["state"], "cleanup-unresolved")

    def test_repeated_signal_during_cleanup_cannot_replace_first_signal(self):
        previous = {
            number: signal.getsignal(number)
            for number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)
        }
        with native.preserve_cancellation() as cancellation:
            handler = signal.getsignal(signal.SIGTERM)
            assert callable(handler)
            with self.assertRaises(SystemExit) as exited:
                handler(signal.SIGTERM, None)
            self.assertEqual(exited.exception.code, 143)
            subsequent = signal.getsignal(signal.SIGHUP)
            assert callable(subsequent)
            subsequent(signal.SIGHUP, None)
            self.assertEqual(cancellation.signum, signal.SIGTERM)
        self.assertEqual({number: signal.getsignal(number) for number in previous}, previous)

    def test_mutation_serialization_refuses_competing_orchestrator(self):
        with tempfile.TemporaryDirectory() as directory:
            first = native.ReceiptStore(Path(directory))
            second = native.ReceiptStore(Path(directory))
            with first.serialized():
                operations = FixtureOperations()
                operations.store = second
                outcome = native.orchestrate(receipt(), second, operations, lambda: None)
                self.assertEqual(outcome.failure, "outcome-pending")
                self.assertEqual(operations.effects, [])
                self.assertFalse(second.exists())

    def test_cleanup_is_idempotent_and_rejects_foreign_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            store = native.ReceiptStore(Path(directory))
            operations = FixtureOperations()
            operations.store = store
            native.orchestrate(receipt(), store, operations, lambda: None)
            removed = list(operations.removed)
            native.cleanup_receipt(store, operations)
            native.cleanup_receipt(store, operations)
            self.assertEqual(operations.removed, removed)
            malformed = store.read()
            malformed["service_label"] += ".foreign"
            (Path(directory) / "receipt.json").write_bytes(native.canonical_json(malformed))
            with self.assertRaises(TaskError):
                native.cleanup_receipt(store, operations)
            self.assertEqual(operations.removed, removed)

    def test_unsupported_macos_never_probes_signatures_or_prepares_execution(self):
        for version in ("25.9", "", "unknown"):
            with (
                self.subTest(version=version),
                patch("platform.system", return_value="Darwin"),
                patch("platform.machine", return_value="arm64"),
                patch("platform.mac_ver", return_value=(version, (), "")),
                patch(
                    "subprocess.Popen",
                    side_effect=AssertionError("unsupported host executed a tool"),
                ),
                self.assertRaisesRegex(TaskError, "^unsupported-host$"),
            ):
                native.NativeCodeVerifier().verify(
                    Path("unused"), native.CODE_IDENTIFIERS["evidence-admin"]
                )

    def test_disposable_wrapper_preserves_local_agent_without_forwarding_or_secrets(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sequence = native.render_runbook(fixtures(root), "3" * 64, disposable=True)
            log = root / "authentication.log"
            for name in ("ssh", "scp"):
                tool = root / name
                tool.write_text(
                    "#!/bin/sh\nset -eu\n"
                    'test "${SSH_AUTH_SOCK-unset}" = /synthetic/local-agent || exit 9\n'
                    f'/usr/bin/printf "%s|%s|%s\\n" "${{SSH_AUTH_SOCK-unset}}" "${{MACOS_SIGNING_CERTIFICATE_PASSWORD-unset}}" "$*" >> "{log}"\n'
                )
                tool.chmod(0o700)
                sequence = sequence.replace(f"/usr/bin/{name}", str(tool))
            environment = {
                **native.native_environment(),
                "SSH_AUTH_SOCK": "/synthetic/local-agent",
                "MACOS_SIGNING_CERTIFICATE_PASSWORD": "must-not-leak",
            }
            result = subprocess.run(
                ["/bin/sh", "-s", "--", "agent-only-destination"],
                input=sequence.encode(),
                capture_output=True,
                env=environment,
                cwd="/",
                timeout=5,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            lines = log.read_text().splitlines()
            self.assertEqual(len(lines), 3)
            for line in lines:
                self.assertTrue(line.startswith("/synthetic/local-agent|unset|"))
                self.assertIn("ForwardAgent=no", line)
                self.assertIn("ForwardX11=no", line)
            self.assertIn("/usr/bin/env -i PATH=", sequence)
            self.assertNotIn("SSH_AUTH_SOCK", sequence.split("SPACETERM_NATIVE_REMOTE'", 1)[1])

    def test_disposable_automation_marks_interactive_cases_unrun(self):
        plan = native.disposable_cases()
        for case in native.INTERACTIVE_CASES:
            self.assertEqual(plan[case], "unrun")
        self.assertEqual(plan["crash-restart"], "required")

    def test_signing_inputs_never_reach_compiler_probe_or_admin_environment(self):
        with patch.dict(
            os.environ,
            {
                "MACOS_SIGNING_CERTIFICATE_PASSWORD": "secret",
                "DYLD_INSERT_LIBRARIES": "/bad",
                "PYTHONPATH": "/bad",
                "HOME": "/private/sensitive",
            },
        ):
            self.assertEqual(
                native.native_environment(),
                {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LC_ALL": "C"},
            )


if __name__ == "__main__":
    unittest.main()
