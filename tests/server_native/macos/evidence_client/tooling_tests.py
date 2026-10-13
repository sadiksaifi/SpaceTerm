"""E4's public task interfaces, with compiler and signing peers injected."""

import importlib.util
import os
import plistlib
import struct
import sys
import tempfile
import unittest
from contextlib import nullcontext
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / "mise-tasks/lib"))
sys.path.insert(0, str(ROOT / "packaging/macos/native-evidence"))
SPEC = importlib.util.spec_from_file_location(
    "build_evidence", ROOT / "mise-tasks/server/native/macos/build_evidence.py"
)
assert SPEC is not None and SPEC.loader is not None
builder = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(builder)

from spaceterm_tasks import TaskError  # noqa: E402


class AssemblyTests(unittest.TestCase):
    def setUp(self):
        fixtures = ROOT / "target/native-evidence-tooling"
        fixtures.mkdir(parents=True, exist_ok=True)
        directory = tempfile.TemporaryDirectory(dir=fixtures)
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.built = self.root / "nondefault-cargo-target"
        self.built.mkdir()
        self.calls = []

    def build_binary(self, *args, binary, env):
        self.calls.append((binary, args, env))
        source = self.built / binary
        source.write_bytes(binary.encode())
        source.chmod(0o755)
        return source

    @staticmethod
    def metadata(*_args):
        return {
            "schema_version": 1,
            "component": "server",
            "build_identity": "preflight",
            "platform": "darwin-arm64",
            "version": "dev." + "1" * 12,
            "build_commit": "1" * 40,
            "protocol_majors": [1, 2],
            "storage": {"read": [1], "write": 1, "migrate_from": [1]},
            "native_identifier": "io.github.sadiksaifi.spaceterm-preflight.server",
            "launcher_abi": 2,
        }

    @staticmethod
    def build_probe(destination, *, environment):
        destination.write_bytes(b"probe")

    @staticmethod
    def build_targets(destination, *, environment):
        app = destination / "SpaceTerm Evidence Companion.app"
        executable = app / "Contents/MacOS/spaceterm-evidence-companion"
        executable.parent.mkdir(parents=True)
        executable.write_bytes(b"target")
        (app / "Contents/Info.plist").write_bytes(
            plistlib.dumps({"CFBundleExecutable": "spaceterm-evidence-companion"})
        )
        return app

    def assemble(self, name="unsigned"):
        # These are public sentinel tokens, never credentials or signing material.
        with patch.dict(
            os.environ,
            {
                "MACOS_SIGNING_CERTIFICATE_P12": "public-signing-sentinel",
                "MACOS_SIGNING_CERTIFICATE_PASSWORD": "public-input-sentinel",
                "SSH_AUTH_SOCK": "public-agent-sentinel",
                "AWS_SESSION_TOKEN": "public-account-sentinel",
                "DYLD_INSERT_LIBRARIES": "public-loader-sentinel",
            },
        ):
            return builder.build_candidate(
                self.root / name,
                build_binary=self.build_binary,
                metadata=self.metadata,
                build_probe=self.build_probe,
                build_targets=self.build_targets,
            )

    def test_cargo_reported_paths_supply_the_complete_product_not_a_synthetic_server(self):
        candidate = self.assemble()
        app = candidate / "server/SpaceTerm Preflight Server.app"
        self.assertEqual(
            (app / "Contents/MacOS/spaceterm-server").read_bytes(), b"spaceterm-server"
        )
        names = {call[0] for call in self.calls}
        self.assertEqual(
            names,
            {
                "spaceterm-server",
                "spaceterm-server-launcher",
                "spaceterm-server-manager",
                "spaceterm-server-spawner",
                "spaceterm-native-evidence-client",
            },
        )
        self.assertFalse((candidate / "evidence-admin").exists())

    def test_builds_receive_no_signing_credential_agent_or_loader_inputs(self):
        self.assemble()
        for _binary, _args, environment in self.calls:
            self.assertFalse(
                {
                    "MACOS_SIGNING_CERTIFICATE_P12",
                    "MACOS_SIGNING_CERTIFICATE_PASSWORD",
                    "SSH_AUTH_SOCK",
                    "AWS_SESSION_TOKEN",
                    "DYLD_INSERT_LIBRARIES",
                }
                & environment.keys()
            )
            self.assertEqual(environment["CARGO_BUILD_JOBS"], "3")

    def test_existing_candidate_is_never_overwritten(self):
        destination = self.root / "unsigned"
        destination.mkdir()
        (destination / "keep").write_bytes(b"existing")
        with self.assertRaisesRegex(TaskError, "unsafe-native-state"):
            self.assemble()
        self.assertEqual((destination / "keep").read_bytes(), b"existing")

    def make_pair(self):
        import assembly

        before = self.assemble("before")
        metadata = self.metadata()
        metadata.update(version="dev." + "2" * 12, build_commit="2" * 40)
        original = self.build_binary

        def compiler(*args, binary, env):
            source = original(*args, binary=binary, env=env)
            if binary == "spaceterm-server":
                source.write_bytes(b"updated-product")
            return source

        with (
            patch.object(self, "metadata", return_value=metadata),
            patch.object(self, "build_binary", side_effect=compiler),
        ):
            after = self.assemble("after")
        assembly.require_update_pair(before, after)
        return before, after

    def test_a_duplicate_build_cannot_be_credited_as_a_signed_update(self):
        import assembly

        candidate = self.assemble()
        with self.assertRaisesRegex(TaskError, "artifact-rejected"):
            assembly.require_update_pair(candidate, candidate)

    def test_signing_uses_the_existing_helper_without_account_inputs(self):
        import assembly

        before, after = self.make_pair()
        with (
            patch.dict(
                os.environ,
                {
                    "AWS_SESSION_TOKEN": "public-account-sentinel",
                    "MACOS_SIGNING_CERTIFICATE_P12": "public-signing-sentinel",
                    "UPDATE_SIGNING_KEY": "public-unneeded-input-sentinel",
                    "APPLE_CERTIFICATE": "public-unneeded-input-sentinel",
                },
            ),
            patch.object(assembly, "release_keychain", return_value=nullcontext()) as helper,
            patch.object(assembly, "_tool", return_value=(b"", b"")) as tool,
            patch.object(assembly, "verify_candidate", side_effect=assembly.read_candidate),
        ):
            signed = assembly.sign_pair(before, after, self.root / "signed")
        self.assertEqual(
            set(helper.call_args.args[1]), {"PATH", "LC_ALL", "MACOS_SIGNING_CERTIFICATE_P12"}
        )
        self.assertTrue(tool.call_args_list)
        self.assertTrue(all(call.args[0][0] == "/usr/bin/codesign" for call in tool.call_args_list))
        self.assertTrue((signed / "baseline.tar.gz").is_file())
        self.assertTrue((signed / "update.tar.gz").is_file())

    def test_signing_finalizes_signature_resource_modes_before_installation_manifest(self):
        import assembly
        from spaceterm_tasks import package_server

        before, after = self.make_pair()

        def signing_peer(arguments, **_options):
            target = Path(arguments[-1])
            if target.suffix == ".app":
                resource = target / "Contents/_CodeSignature/CodeResources"
                resource.parent.mkdir()
                resource.write_bytes(b"public-signature-resource")
                resource.chmod(0o600)
            return b"", b""

        with (
            patch.object(assembly, "release_keychain", return_value=nullcontext()),
            patch.object(assembly, "_tool", side_effect=signing_peer),
            patch.object(assembly, "verify_candidate", side_effect=assembly.read_candidate),
        ):
            signed = assembly.sign_pair(before, after, self.root / "signed-resources")
        for phase in ("baseline", "update"):
            manifest = package_server.verify_archive(signed / f"{phase}.tar.gz")
            resource = (
                signed
                / phase
                / "server/SpaceTerm Preflight Server.app/Contents/_CodeSignature/CodeResources"
            )
            self.assertEqual(resource.stat().st_mode & 0o777, 0o444)
            entry = next(
                item for item in manifest["files"] if item["name"].endswith("/CodeResources")
            )
            self.assertEqual(entry["mode"], 0o444)

    def test_preparation_copies_an_injected_pair_and_generates_uncredited_runbooks(self):
        import assembly
        import sequence

        before, after = self.make_pair()
        with (
            patch.object(assembly, "release_keychain", return_value=nullcontext()),
            patch.object(assembly, "_tool", return_value=(b"", b"")),
            patch.object(assembly, "verify_candidate", side_effect=assembly.read_candidate),
        ):
            signed = assembly.sign_pair(before, after, self.root / "signed")
        preflight = sequence.prepare_pair(
            signed, self.root / "prepared", verify=assembly.read_candidate
        )
        self.assertEqual(preflight["native_passed"], 0)
        self.assertEqual(preflight["production_admission"], "uncredited")
        self.assertTrue((self.root / "prepared/local-sequence.md").is_file())
        self.assertEqual(sequence.read_preflight(self.root / "prepared"), preflight)
        self.assertFalse((self.root / "prepared/evidence-admin").exists())

    def test_replaced_and_restored_artifacts_fail_before_preflight_credit(self):
        import assembly

        candidate = self.assemble()

        class SubstituteVerifier:
            def verify(self, path, _identifier):
                original = path.read_bytes()
                path.unlink()
                path.write_bytes(original)
                path.chmod(0o555)

        with self.assertRaisesRegex(TaskError, "artifact-rejected"):
            assembly.verify_candidate(candidate, verifier=SubstituteVerifier())

    def test_logout_resume_keeps_prior_notes_and_rejects_restart_at_step_one(self):
        import sequence

        identity = {"run_id": "1" * 32, "artifact_sha256": "2" * 64}
        file = self.root / "state/owner-outcomes.jsonl"
        self.assertEqual(sequence.checkpoint(file, identity), (1,))
        self.assertEqual(sequence.checkpoint(file, identity, 1, "done"), (2,))
        retained = file.read_bytes()
        self.assertEqual(sequence.checkpoint(file, identity), (2,))
        with self.assertRaisesRegex(TaskError, "invalid-record"):
            sequence.checkpoint(file, identity, 1, "done")
        self.assertEqual(file.read_bytes(), retained)
        different = {"run_id": "3" * 32, "artifact_sha256": "2" * 64}
        with self.assertRaisesRegex(TaskError, "invalid-record"):
            sequence.checkpoint(file, different)

    def test_background_items_owner_steps_share_one_recorded_interaction(self):
        import sequence

        identity = {"run_id": "1" * 32, "artifact_sha256": "2" * 64}
        file = self.root / "state/owner-outcomes.jsonl"
        for step in (1, 2, 3, 4, 12, 13, 8):
            sequence.checkpoint(
                file,
                identity,
                step,
                "done",
                authentication=("touch-id", "password") if step == 13 else None,
            )
        records = sequence.read_outcomes(file, identity)
        self.assertEqual(records[-1]["owner_steps"], [8, 14])
        self.assertEqual(sequence.checkpoint(file, identity), (9,))

    def test_local_wire_comparison_preserves_the_reference_and_parenting_context(self):
        import observations

        identity = {
            "run_id": "1" * 32,
            "revision": "2" * 40,
            "dirty": True,
            "artifact_sha256": "3" * 64,
        }
        host = {
            "platform": "macos",
            "architecture": "arm64",
            "os_version": [26, 6, 0],
            "kernel_version": [25, 6, 0],
            "libc_version": None,
            "supervisor": "launchd",
            "supervisor_version": None,
        }
        gui = observations.wire_record(struct.pack(">5I", 2, 1, 1, 2, 0), identity, host)
        daemon = observations.wire_record(struct.pack(">5I", 2, 2, 1, 2, 0), identity, host)
        self.assertEqual((gui["context"], daemon["context"]), ("gui-parented", "daemon-parented"))
        self.assertEqual(gui["facts"]["reference"], "sanitized-launch-v1")
        self.assertEqual(gui["result"], "pending")
        self.assertFalse(gui["facts"]["startup_order_verified"])

    def test_partial_collection_runs_without_filling_missing_or_foreign_records(self):
        import observations

        spec = importlib.util.spec_from_file_location(
            "collect_evidence", ROOT / "mise-tasks/server/native/macos/collect_evidence.py"
        )
        assert spec is not None and spec.loader is not None
        collector = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(collector)
        identity = {
            "run_id": "1" * 32,
            "revision": "2" * 40,
            "dirty": True,
            "artifact_sha256": "3" * 64,
        }
        host = {
            "platform": "macos",
            "architecture": "arm64",
            "os_version": [26, 6, 0],
            "kernel_version": [25, 6, 0],
            "libc_version": None,
            "supervisor": "launchd",
            "supervisor_version": None,
        }
        paths = [self.root / "gui.jsonl", self.root / "daemon.jsonl"]
        for context, path in enumerate(paths, start=1):
            observations.write_wire_record(
                path, struct.pack(">5I", 2, context, 1, 2, 0), identity, host
            )
        summary = collector.collect_inputs(paths, identity, partial=True)
        self.assertEqual((summary.records, summary.passed, summary.unresolved), (2, 0, 2))
        with self.assertRaisesRegex(TaskError, "identity-mismatch"):
            collector.collect_inputs(paths, {**identity, "run_id": "4" * 32}, partial=True)
        with self.assertRaisesRegex(TaskError, "invalid-record"):
            collector.collect_inputs([paths[0], paths[0]], identity, partial=True)

    def test_zero_effect_install_refusal_allows_one_retained_retry_not_resolution(self):
        import sequence

        identity = {"run_id": "1" * 32, "artifact_sha256": "2" * 64}
        for outcome in ("denied", "cancelled"):
            file = self.root / outcome / "owner-outcomes.jsonl"
            self.assertEqual(sequence.checkpoint(file, identity, 1, outcome), (1,))
            retained = file.read_bytes()
            self.assertEqual(sequence.checkpoint(file, identity), (1,))
            with self.assertRaisesRegex(TaskError, "invalid-record"):
                sequence.checkpoint(file, identity, 1, "done", resolve=True)
            self.assertEqual(sequence.checkpoint(file, identity, 1, "done"), (2,))
            self.assertTrue(file.read_bytes().startswith(retained))
            self.assertEqual(len(sequence.read_outcomes(file, identity)), 2)
            with self.assertRaisesRegex(TaskError, "invalid-record"):
                sequence.checkpoint(file, identity, 1, "done")
        file = self.root / "twice" / "owner-outcomes.jsonl"
        sequence.checkpoint(file, identity, 1, "cancelled")
        self.assertIsNone(sequence.checkpoint(file, identity, 1, "denied"))
        with self.assertRaisesRegex(TaskError, "outcome-pending"):
            sequence.checkpoint(file, identity, 1, "done")

    def test_touch_id_checkpoint_retains_bounded_method_for_each_context(self):
        import sequence

        identity = {"run_id": "1" * 32, "artifact_sha256": "2" * 64}
        file = self.root / "authentication/owner-outcomes.jsonl"
        for step in (1, 2, 3, 4, 12):
            sequence.checkpoint(file, identity, step, "done")
        with self.assertRaisesRegex(TaskError, "invalid-record"):
            sequence.checkpoint(file, identity, 13, "done")
        with self.assertRaisesRegex(TaskError, "invalid-record"):
            sequence.checkpoint(file, identity, 13, "done", authentication=("unknown", "password"))
        self.assertEqual(
            sequence.checkpoint(
                file, identity, 13, "done", authentication=("touch-id", "password")
            ),
            (8, 14),
        )
        note = sequence.read_outcomes(file, identity)[-1]
        self.assertEqual(
            note["authentication"], {"gui-parented": "touch-id", "daemon-parented": "password"}
        )

    def test_cleanup_uncertainty_and_failed_install_halt_checkpoint_advancement(self):
        import sequence

        identity = {"run_id": "1" * 32, "artifact_sha256": "2" * 64}
        for outcome in ("unavailable", "cleanup-unresolved"):
            file = self.root / outcome / "owner-outcomes.jsonl"
            self.assertIsNone(sequence.checkpoint(file, identity, 1, outcome))
            retained = file.read_bytes()
            self.assertIsNone(sequence.checkpoint(file, identity))
            with self.assertRaisesRegex(TaskError, "outcome-pending"):
                sequence.checkpoint(file, identity, 2, "done")
            self.assertEqual(file.read_bytes(), retained)
        file = self.root / "later" / "owner-outcomes.jsonl"
        sequence.checkpoint(file, identity, 1, "done")
        self.assertIsNone(sequence.checkpoint(file, identity, 2, "cleanup-unresolved"))
        self.assertIsNone(sequence.checkpoint(file, identity))

    def test_default_checkpoint_path_preserves_the_documented_state_across_resume(self):
        import sequence

        with patch.dict(os.environ, {"HOME": str(self.root)}):
            file = sequence.default_state("1" * 32)
            self.assertEqual(
                file,
                self.root
                / "Library/Application Support/SpaceTerm Native Evidence 520"
                / ("1" * 32)
                / "owner-outcomes.jsonl",
            )
            identity = {"run_id": "1" * 32, "artifact_sha256": "2" * 64}
            sequence.checkpoint(file, identity, 1, "done")
            self.assertEqual(sequence.checkpoint(sequence.default_state("1" * 32), identity), (2,))

    def test_raw_passed_wire_cannot_receive_native_credit_for_a_verified_pair(self):
        import observations

        identity = {
            "run_id": "1" * 32,
            "revision": "2" * 40,
            "dirty": True,
            "artifact_sha256": "3" * 64,
        }
        host = {
            "platform": "macos",
            "architecture": "arm64",
            "os_version": [26, 6, 0],
            "kernel_version": [25, 6, 0],
            "libc_version": None,
            "supervisor": "launchd",
            "supervisor_version": None,
        }
        wire = struct.pack(">8I", 1, 5, 1, 1, 1, 1, 1, 0)
        for current in (identity, {**identity, "run_id": "4" * 32, "revision": "5" * 40}):
            record = observations.wire_record(wire, current, host)
            self.assertEqual(record["proof"], "synthetic")
            self.assertEqual(record["result"], "pending")
            self.assertEqual(record["failure"], "execution-binding-unproven")

    def test_verification_and_sign_pair_isolate_actual_closure_inspection_children(self):
        import subprocess

        import assembly

        before, after = self.make_pair()
        # The wrapper replaces only external signature/metadata peers. It runs
        # the real closure verifier and its actual subprocess in the child.
        wrapper = self.root / "inspection-peer.py"
        wrapper.write_text("""
import os, sys, subprocess
from pathlib import Path
sys.path.insert(0, sys.argv[1])
sys.path.insert(0, sys.argv[2])
import assembly
from spaceterm_tasks import package_server_macos as closure
from spaceterm_tasks.macos_signing import MACH_O_MAGIC
candidate = sys.argv[3]
root = Path(sys.argv[4])
code = {root / 'probe'}
root.mkdir(exist_ok=True)
(root / 'probe').write_bytes(next(iter(MACH_O_MAGIC)))
def checked(arguments, *_args, **options):
    subprocess.run([sys.executable, '-I', '-c', "import os; assert set(os.environ) <= {'PATH', 'LC_ALL', '__CF_USER_TEXT_ENCODING'}"], env=options['env'], check=True, capture_output=True)
    return 'Load command 0\\n cmd LC_LOAD_DYLIB\\n name /usr/lib/libSystem.B.dylib (offset 24)\\n'
closure.checked = checked
original = closure.verify_macho_closure
closure.verify_macho_closure = lambda *_args: original(root, code)
assembly.package_server.verify_executable_metadata = lambda *_args: None
import runpy
worker = str(Path(assembly.__file__).with_name('inspect_candidate.py'))
sys.argv = [worker, candidate]
runpy.run_path(worker, run_name='__main__')
""")
        for index in range(1, 4):
            (self.root / f"inspection-{index}").mkdir()
        original_tool = assembly._tool
        inspection_calls = []
        original_closure = assembly.package_server_macos.verify_macho_closure

        def inherited_inspection(_root, _code):
            from spaceterm_tasks.macos_signing import MACH_O_MAGIC

            root = self.root / "inherited-inspection"
            root.mkdir()
            executable = root / "probe"
            executable.write_bytes(next(iter(MACH_O_MAGIC)))

            def checked(_arguments, *_args, **options):
                subprocess.run(
                    [
                        sys.executable,
                        "-I",
                        "-c",
                        "import os; assert set(os.environ) <= {'PATH', 'LC_ALL', '__CF_USER_TEXT_ENCODING'}",
                    ],
                    env=options["env"],
                    check=True,
                    capture_output=True,
                )
                return "Load command 0\n cmd LC_LOAD_DYLIB\n name /usr/lib/libSystem.B.dylib (offset 24)\n"

            with patch.object(assembly.package_server_macos, "checked", side_effect=checked):
                original_closure(root, {executable})

        def tool(arguments, timeout=10):
            if "inspect_candidate.py" in str(arguments[2:3]):
                inspection_calls.append(arguments)
                return original_tool(
                    [
                        sys.executable,
                        "-I",
                        str(wrapper),
                        str(ROOT / "mise-tasks/lib"),
                        str(ROOT / "packaging/macos/native-evidence"),
                        arguments[3],
                        str(self.root / f"inspection-{len(inspection_calls)}"),
                    ],
                    timeout=timeout,
                )
            return b"", b""

        class Verifier:
            def verify(self, *_args):
                pass

        with (
            patch.dict(
                os.environ,
                {
                    "AWS_SESSION_TOKEN": "public-account-sentinel",
                    "SSH_AUTH_SOCK": "public-agent-sentinel",
                    "MACOS_SIGNING_CERTIFICATE_P12": "public-signing-sentinel",
                },
            ),
            patch.object(assembly, "_tool", side_effect=tool),
            patch.object(assembly, "NativeCodeVerifier", return_value=Verifier()),
            patch.object(assembly.package_server_macos, "verify_info_plist"),
            patch.object(assembly.package_server, "verify_executable_metadata"),
            patch.object(
                assembly.package_server_macos,
                "verify_macho_closure",
                side_effect=inherited_inspection,
            ),
            patch.object(assembly, "release_keychain", return_value=nullcontext()),
        ):
            assembly.verify_candidate(before)
            assembly.sign_pair(before, after, self.root / "inspected-pair")
        self.assertEqual(len(inspection_calls), 3)

    def test_preparation_uses_signature_verification_and_refuses_a_failed_signature(self):
        import assembly
        import sequence

        before, after = self.make_pair()
        with (
            patch.object(assembly, "release_keychain", return_value=nullcontext()),
            patch.object(assembly, "_tool", return_value=(b"", b"")),
            patch.object(assembly, "verify_candidate", side_effect=assembly.read_candidate),
        ):
            signed = assembly.sign_pair(before, after, self.root / "signed")

        def reject_signature(*_args):
            raise TaskError("artifact-rejected")

        with (
            patch(
                "spaceterm_tasks.server_native_macos._tool", side_effect=reject_signature
            ) as native_tool,
            patch("spaceterm_tasks.server_native_macos.platform.system", return_value="Darwin"),
            patch("spaceterm_tasks.server_native_macos.platform.machine", return_value="arm64"),
            patch(
                "spaceterm_tasks.server_native_macos.platform.mac_ver",
                return_value=("26.0", (), ""),
            ),
        ):
            with self.assertRaisesRegex(TaskError, "artifact-rejected"):
                sequence.prepare_pair(signed, self.root / "rejected")
        self.assertEqual(native_tool.call_args.args[0][:2], ["/usr/bin/codesign", "--verify"])
        self.assertFalse((self.root / "rejected/preflight.json").exists())

    @staticmethod
    def collection_task():
        specification = importlib.util.spec_from_file_location(
            "e4_collect", ROOT / "mise-tasks/server/native/macos/collect_evidence.py"
        )
        assert specification is not None and specification.loader is not None
        module = importlib.util.module_from_spec(specification)
        specification.loader.exec_module(module)
        return module

    def native_capability_part(self, name, identity, *, capability=5, result="passed", context=1):
        import observations
        from spaceterm_tasks.native_evidence_schema import encode_record

        host = {
            "platform": "macos",
            "architecture": "arm64",
            "os_version": [26, 6, 0],
            "kernel_version": [25, 6, 0],
            "libc_version": None,
            "supervisor": "launchd",
            "supervisor_version": None,
        }
        record = observations.wire_record(
            struct.pack(">8I", 1, capability, context, 1, context, 1, 1, 0), identity, host
        )
        # An injected native producer supplies these test-only observations.
        # The standalone wire importer cannot promote its own output this way.
        record.update(proof="native", result=result, failure=None)
        if result == "unavailable":
            record["failure"] = "native-unavailable"
            record["facts"].update(effect="unavailable", preflight="unavailable")
        path = self.root / name
        path.write_bytes(encode_record(record))
        return path

    def test_final_collection_is_incomplete_without_owner_lifecycle_and_publication_cases(self):
        collector = self.collection_task()
        identity = {
            "run_id": "1" * 32,
            "revision": "2" * 40,
            "dirty": True,
            "artifact_sha256": "3" * 64,
        }
        part = self.native_capability_part("matrix.jsonl", identity)
        summary = collector.collect_inputs([part], identity)
        self.assertGreaterEqual(summary.unrun, 6)
        required = {(case.job, case.case) for case in collector.required_cases("local")}
        for case in (
            "boot-before-login",
            "running-survives-logout",
            "stopped-survives-reboot",
            "storage-unlock-recovery",
        ):
            self.assertIn(("native-installation", case), required)
        self.assertIn(("macos-management", "background-items-suppression"), required)
        self.assertEqual(
            collector.PUBLICATION_CASE,
            "publication-authenticates-protected-copies-before-root-exec",
        )

    def test_retained_before_and_after_parts_validate_update_revision_after_checkpoint_only(self):
        import sequence

        collector = self.collection_task()
        identity = {
            "run_id": "1" * 32,
            "revision": "2" * 40,
            "dirty": True,
            "artifact_sha256": "3" * 64,
            "baseline_revision": "2" * 40,
            "baseline_dirty": True,
            "update_revision": "4" * 40,
            "update_dirty": False,
        }
        before = self.native_capability_part("before.jsonl", identity, context=2)
        updated_identity = {**identity, "revision": identity["update_revision"], "dirty": False}
        after = self.native_capability_part("after.jsonl", updated_identity, context=2)
        activation = self.native_activation_part(updated_identity)
        retained = before.read_bytes()
        state = self.root / "owner-outcomes.jsonl"
        with self.assertRaisesRegex(TaskError, "outcome-pending"):
            collector.collect_inputs(
                [before], identity, partial=True, after_update_inputs=[after], owner_outcomes=state
            )
        for steps in sequence.STEPS:
            sequence.checkpoint(
                state,
                identity,
                steps[0],
                "done",
                authentication=("touch-id", "password") if steps == (13,) else None,
            )
            if steps == (10,):
                break
        summary = collector.collect_inputs(
            [before],
            identity,
            partial=True,
            after_update_inputs=[after, activation],
            owner_outcomes=state,
        )
        self.assertEqual((summary.records, summary.passed, summary.retention), (3, 3, "passed"))
        self.assertEqual(before.read_bytes(), retained)
        with self.assertRaisesRegex(TaskError, "identity-mismatch"):
            collector.collect_inputs([after], identity, partial=True)
        with self.assertRaisesRegex(TaskError, "invalid-record"):
            collector.collect_inputs([before, before], identity, partial=True)
        with self.assertRaisesRegex(TaskError, "identity-mismatch"):
            collector.collect_inputs(
                [before], identity, partial=True, after_update_inputs=[before], owner_outcomes=state
            )
        refused = self.native_capability_part(
            "after-unavailable.jsonl",
            {**identity, "revision": identity["update_revision"], "dirty": False},
            result="unavailable",
        )
        summary = collector.collect_inputs(
            [before], identity, partial=True, after_update_inputs=[refused], owner_outcomes=state
        )
        self.assertEqual(summary.retention, "pending")
        self.assertGreater(summary.unresolved, 0)

    def test_reviewed_resolution_retains_halted_notes_and_resumes_only_the_named_step(self):
        import sequence

        identity = {"run_id": "1" * 32, "artifact_sha256": "2" * 64}
        file = self.root / "resolved/owner-outcomes.jsonl"
        sequence.checkpoint(file, identity, 1, "done")
        sequence.checkpoint(file, identity, 2, "cleanup-unresolved")
        retained = file.read_bytes()
        with self.assertRaisesRegex(TaskError, "invalid-record"):
            sequence.checkpoint(file, identity, 3, "done", resolve=True)
        self.assertEqual(sequence.checkpoint(file, identity, 2, "done", resolve=True), (3,))
        self.assertTrue(file.read_bytes().startswith(retained))
        self.assertEqual(
            sequence.read_outcomes(file, identity)[-1]["scope"], "coordinator-resolution"
        )
        self.assertEqual(sequence.checkpoint(file, identity), (3,))

    def test_run_task_reports_halted_state_and_collection_never_credits_missing_cases(self):
        import io
        from contextlib import redirect_stdout

        specification = importlib.util.spec_from_file_location(
            "e4_run", ROOT / "mise-tasks/server/native/macos/run_evidence.py"
        )
        assert specification is not None and specification.loader is not None
        run = importlib.util.module_from_spec(specification)
        specification.loader.exec_module(run)
        identity = {
            "run_id": "1" * 32,
            "revision": "2" * 40,
            "dirty": True,
            "artifact_sha256": "3" * 64,
        }
        state = self.root / "owner-outcomes.jsonl"
        output = io.StringIO()
        with (
            patch.object(
                sys,
                "argv",
                [
                    "run-evidence",
                    "--state",
                    str(state),
                    "--step",
                    "1",
                    "--outcome",
                    "cleanup-unresolved",
                ],
            ),
            patch.object(run, "read_preflight", return_value=identity),
            patch.object(run, "verify_candidate"),
            redirect_stdout(output),
        ):
            self.assertEqual(run.entry(), 2)
        self.assertIn("classification=recovery-required", output.getvalue())
        self.assertIn(str(state), output.getvalue())
        self.assertNotIn("next_step=2", output.getvalue())
        for outcome, classification, status in (
            ("denied", "retry-install", 0),
            ("unavailable", "run-ended", 2),
        ):
            output = io.StringIO()
            with (
                patch.object(
                    sys,
                    "argv",
                    [
                        "run-evidence",
                        "--state",
                        str(self.root / outcome),
                        "--step",
                        "1",
                        "--outcome",
                        outcome,
                    ],
                ),
                patch.object(run, "read_preflight", return_value=identity),
                patch.object(run, "verify_candidate"),
                redirect_stdout(output),
            ):
                self.assertEqual(run.entry(), status)
            self.assertIn(f"classification={classification}", output.getvalue())
            self.assertNotIn("recovery-required", output.getvalue())
            self.assertNotIn("next_step=2", output.getvalue())
        collector = self.collection_task()
        part = self.native_capability_part("incomplete.jsonl", identity)
        output = io.StringIO()
        with (
            patch.object(
                sys,
                "argv",
                ["collect-evidence", "--input", str(part), "--candidate", str(self.root)],
            ),
            patch.object(collector, "read_preflight", return_value=identity),
            patch.object(collector, "verify_candidate"),
            redirect_stdout(output),
        ):
            self.assertEqual(collector.entry(), 2)
        self.assertIn("classification=incomplete", output.getvalue())

    def test_each_pty_setup_defines_only_variables_without_changing_path_or_directory(self):
        import subprocess

        text = (ROOT / "packaging/macos/native-evidence/local-sequence.md").read_text()
        section = text.split("## Variables for each PTY\n", 1)[-1]
        block = section.split("```sh\n", 1)[1].split("```", 1)[0]
        replacements = {"@RUN_ID@": "1" * 32, "@STATE@": str(self.root / "run-state")}
        for token, value in replacements.items():
            block = block.replace(token, value)
        shell = "/bin/zsh" if sys.platform == "darwin" else "/bin/bash"
        script = (
            block
            + '\n[ -n "$CLIENT" ] && [ -n "$RUN_ID" ] && [ -n "$STATE" ] || exit 91\n[ "$PATH" = /usr/bin:/bin ] || exit 92\n[ "$PWD" = "$EXPECTED_DIRECTORY" ] || exit 93\n'
        )
        result = subprocess.run(
            [shell, "-c", script],
            cwd=self.root,
            env={
                "PATH": "/usr/bin:/bin",
                "HOME": str(self.root),
                "EXPECTED_DIRECTORY": str(self.root),
            },
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_jammy_sequence_stops_on_install_and_middle_suite_failures(self):
        import subprocess

        text = (ROOT / "packaging/macos/native-evidence/jammy-sequence.md").read_text()
        block = next(
            part.split("```", 1)[0]
            for part in text.split("```sh\n")[1:]
            if "custody:install:linux" in part.split("```", 1)[0]
        )
        checkout = self.root / "guest-checkout"
        tasks = checkout / "mise-tasks/server/vm/linux"
        tasks.mkdir(parents=True)
        for name in ("install", "verify", "lifecycle", "manage", "reboot", "boot_check", "cleanup"):
            (tasks / f"custody_{name}.py").touch()
        tools = self.root / "guest-tools"
        tools.mkdir()
        runner = tools / "mise"
        runner.write_text(
            '#!/bin/sh\nprintf "%s\\n" "$*" >> "$CALLS"\ncase "$*" in *"$FAIL_TASK"*) exit 17;; esac\n'
        )
        runner.chmod(0o755)
        for failed, expected in (("custody:install:linux", 1), ("custody:verify:linux aliases", 4)):
            calls = self.root / f"calls-{expected}"
            result = subprocess.run(
                ["/bin/sh", "-c", block],
                cwd=checkout,
                env={
                    "PATH": str(tools) + ":/usr/bin:/bin",
                    "CALLS": str(calls),
                    "FAIL_TASK": failed,
                },
                capture_output=True,
                check=False,
            )
            self.assertEqual(result.returncode, 17)
            invoked = calls.read_text().splitlines()
            self.assertEqual(len(invoked), expected)
            self.assertIn(failed, invoked[-1])
            self.assertIn(b"recovery-required", result.stderr)

    def test_clipboard_unavailable_input_has_no_gui_target_or_paste_instruction(self):
        # The signed client handles case 6 as a non-GUI descriptor. The native
        # probe refuses acquisition on this profile; no marker is copied.
        text = (ROOT / "packaging/macos/native-evidence/local-sequence.md").read_text()
        self.assertNotIn("Use case 4, 5, 6, 7 or 8", text)
        self.assertNotIn("dashboard's Run Probe", text)
        self.assertNotIn("restore the four shell variables", text)

    def update_state(self, identity, name="update-outcomes.jsonl"):
        import sequence

        state = self.root / name
        for steps in sequence.STEPS:
            sequence.checkpoint(
                state,
                identity,
                steps[0],
                "done",
                authentication=("touch-id", "password") if steps == (13,) else None,
            )
            if steps == (10,):
                break
        return state

    def native_activation_part(self, identity):
        import observations
        from spaceterm_tasks.native_evidence_schema import FACT_FIELDS, encode_record

        host = {
            "platform": "macos",
            "architecture": "arm64",
            "os_version": [26, 6, 0],
            "kernel_version": [25, 6, 0],
            "libc_version": None,
            "supervisor": "launchd",
            "supervisor_version": None,
        }
        record = observations.wire_record(
            struct.pack(">8I", 1, 5, 2, 1, 2, 1, 1, 0), identity, host
        )
        record.update(
            job="macos-management",
            case="fresh-signed-update",
            proof="native",
            result="passed",
            failure=None,
        )
        record["facts"] = {"kind": "update", **dict.fromkeys(FACT_FIELDS["update"].split(), True)}
        path = self.root / "activation.jsonl"
        path.write_bytes(encode_record(record))
        return path

    def test_server_privacy_retention_refuses_desktop_only_evidence(self):
        collector = self.collection_task()
        identity = {
            "run_id": "1" * 32,
            "revision": "2" * 40,
            "dirty": False,
            "artifact_sha256": "3" * 64,
            "update_revision": "4" * 40,
            "update_dirty": False,
        }
        update = {**identity, "revision": "4" * 40}
        before = self.native_capability_part("desktop-before.jsonl", identity)
        after = self.native_capability_part("desktop-after.jsonl", update)
        activation = self.native_activation_part(update)
        summary = collector.collect_inputs(
            [before],
            identity,
            partial=True,
            after_update_inputs=[after, activation],
            owner_outcomes=self.update_state(identity),
        )
        self.assertNotEqual(summary.retention, "passed")
        self.assertGreater(summary.unresolved, 0)

    def test_server_privacy_retention_requires_native_activation_and_unchanged_server_responsibility(
        self,
    ):
        import json

        from spaceterm_tasks.native_evidence_schema import encode_record

        collector = self.collection_task()
        identity = {
            "run_id": "1" * 32,
            "revision": "2" * 40,
            "dirty": False,
            "artifact_sha256": "3" * 64,
            "update_revision": "4" * 40,
            "update_dirty": False,
        }
        update = {**identity, "revision": "4" * 40}
        before = self.native_capability_part("server-before.jsonl", identity, context=2)
        after = self.native_capability_part("server-after.jsonl", update, context=2)
        state = self.update_state(identity)
        summary = collector.collect_inputs(
            [before], identity, partial=True, after_update_inputs=[after], owner_outcomes=state
        )
        self.assertNotEqual(summary.retention, "passed")
        activation = self.native_activation_part(update)
        summary = collector.collect_inputs(
            [before],
            identity,
            partial=True,
            after_update_inputs=[after, activation],
            owner_outcomes=state,
        )
        self.assertEqual(summary.retention, "passed")
        substituted = json.loads(after.read_bytes())
        substituted["facts"]["responsible_process"] = "desktop"
        after.write_bytes(encode_record(substituted))
        summary = collector.collect_inputs(
            [before],
            identity,
            partial=True,
            after_update_inputs=[after, activation],
            owner_outcomes=state,
        )
        self.assertNotEqual(summary.retention, "passed")
        before.write_bytes(encode_record({**substituted, "revision": "2" * 40}))
        summary = collector.collect_inputs(
            [before],
            identity,
            partial=True,
            after_update_inputs=[after, activation],
            owner_outcomes=state,
        )
        self.assertNotEqual(summary.retention, "passed")

    def test_screen_recording_retention_requires_an_actual_after_update_effect(self):
        import sequence

        collector = self.collection_task()
        identity = {
            "run_id": "1" * 32,
            "revision": "2" * 40,
            "dirty": True,
            "artifact_sha256": "3" * 64,
            "baseline_revision": "2" * 40,
            "baseline_dirty": True,
            "update_revision": "4" * 40,
            "update_dirty": True,
        }
        before = self.native_capability_part(
            "screen-before.jsonl", identity, capability=4, context=2
        )
        updated_identity = {**identity, "revision": "4" * 40}
        after = self.native_capability_part(
            "screen-after.jsonl", updated_identity, capability=4, context=2
        )
        activation = self.native_activation_part(updated_identity)
        state = self.root / "update-screen-outcomes.jsonl"
        for steps in sequence.STEPS:
            sequence.checkpoint(
                state,
                identity,
                steps[0],
                "done",
                authentication=("touch-id", "password") if steps == (13,) else None,
            )
            if steps == (10,):
                break
        result = collector.collect_inputs(
            [before],
            identity,
            partial=True,
            after_update_inputs=[after, activation],
            owner_outcomes=state,
        )
        self.assertEqual(result.retention, "passed")
        unavailable = self.native_capability_part(
            "screen-unavailable.jsonl",
            {**identity, "revision": "4" * 40},
            capability=4,
            result="unavailable",
        )
        result = collector.collect_inputs(
            [before],
            identity,
            partial=True,
            after_update_inputs=[unavailable],
            owner_outcomes=state,
        )
        self.assertEqual(result.retention, "pending")

    def test_environment_collection_requires_complete_native_startup_order_observations(self):
        import observations
        from spaceterm_tasks.native_evidence_schema import encode_record

        collector = self.collection_task()
        identity = {
            "run_id": "1" * 32,
            "revision": "2" * 40,
            "dirty": True,
            "artifact_sha256": "3" * 64,
        }
        capability = self.native_capability_part("shell.jsonl", identity, capability=13)
        host = {
            "platform": "macos",
            "architecture": "arm64",
            "os_version": [26, 6, 0],
            "kernel_version": [25, 6, 0],
            "libc_version": None,
            "supervisor": "launchd",
            "supervisor_version": None,
        }
        comparison = observations.wire_record(struct.pack(">5I", 2, 1, 1, 2, 0), identity, host)
        comparison["proof"] = "native"
        pending = self.root / "pending-comparison.jsonl"
        pending.write_bytes(encode_record(comparison))
        with self.assertRaisesRegex(TaskError, "outcome-pending"):
            collector.collect_inputs([capability, pending], identity, partial=True)
        # Only an injected startup-order producer supplies this decisive fact.
        comparison.update(result="passed", failure=None)
        comparison["facts"]["startup_order_verified"] = True
        complete = self.root / "complete-comparison.jsonl"
        complete.write_bytes(encode_record(comparison))
        result = collector.collect_inputs([capability, complete], identity, partial=True)
        self.assertEqual((result.passed, result.unresolved), (2, 0))


if __name__ == "__main__":
    unittest.main()
