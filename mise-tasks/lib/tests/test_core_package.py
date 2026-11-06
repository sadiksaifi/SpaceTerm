"""Signed server archives are authenticated before layout or executable inspection."""

import base64
import io
import json
import os
import re
import shutil
import stat
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from spaceterm_tasks import ROOT, TaskError, core_package, update_signing
from spaceterm_tasks.package_linux import write_archive
from spaceterm_tasks.packaging import NOTICES, SHELL_INTEGRATION, TERMINFO

from tests.core_artifact_fixture import archive_tree, populate, seal_foreign


class ResourcePolicyTests(unittest.TestCase):
    def test_policy_matches_tracked_assets(self):
        tracked = subprocess.run(
            ["git", "ls-files", "assets/shell-integration", "assets/THIRD-PARTY-NOTICES.txt"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        ).stdout.splitlines()
        self.assertCountEqual(
            json.loads((ROOT / "packaging/core-resources.json").read_text()),
            ["share/spaceterm/" + name.removeprefix("assets/") for name in tracked],
        )


class CoreBundleNameTests(unittest.TestCase):
    def test_each_identity_names_its_core_bundle_and_identifier(self):
        self.assertEqual(
            [
                (core_package.bundle_directory(identity), core_package.core_identifier(identity))
                for identity in ("spaceterm", "spaceterm-preflight", "spaceterm-development")
            ],
            [
                ("SpaceTerm Core.app", "com.peblon.spaceterm.core"),
                ("SpaceTerm Core Preflight.app", "com.peblon.spaceterm-preflight.core"),
                (
                    "SpaceTerm Core Development.app",
                    "com.peblon.spaceterm-development.core",
                ),
            ],
        )


class CoreArchiveTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.seed = core_package.artifact_signing_seed("spaceterm-preflight")
        # The release trust: a current and a backup key that no nonproduction seed matches.
        self.public = update_signing.derived_public_key(bytes(range(32)))
        trust = patch.object(
            update_signing,
            "release_trust",
            return_value=(self.public, update_signing.derived_public_key(bytes(range(1, 33)))),
        )
        trust.start()
        self.addCleanup(trust.stop)
        self.archive = self.root / "server.tar.gz"
        self.signature = self.root / "server.tar.gz.sig"
        self.commit = "a" * 40
        self.info = {
            "schema": 1,
            "stapled": False,
            "source_sha256": None,
            "protocols": core_package.PROTOCOLS,
            "identity": "spaceterm-preflight",
            "target": "x86_64-unknown-linux-gnu",
            "commit": self.commit,
            "version": "dev.aaaaaaaaaaaa",
            "floor": None,
        }

    def test_signed_stapled_release_requires_exact_ticket_layout_independent_of_dry_run(self):
        target = "aarch64-apple-darwin"
        ticket = "SpaceTerm Core.app/Contents/CodeResources"
        for stapled, present in ((True, True), (False, False), (True, False), (False, True)):
            with self.subTest(stapled=stapled, present=present):
                info = {
                    **self.info,
                    "identity": "spaceterm",
                    "target": target,
                    "version": "1.0.0",
                    "floor": "1.0.0",
                    "stapled": stapled,
                }
                tree = self.root / f"{stapled}-{present}" / "core"
                executable = tree / core_package.executable_path(target, "spaceterm")
                executable.parent.mkdir(parents=True)
                executable.write_bytes(
                    bytes.fromhex("cffaedfe0c0000010000000002000000") + bytes(48)
                )
                executable.chmod(0o755)
                populate(ROOT, tree, info)
                seal_foreign(tree, info)
                (tree / ticket).unlink(missing_ok=True)
                if present:
                    (tree / ticket).write_bytes(b"stapled ticket")
                archive, signature = archive_tree(ROOT, tree, info)
                # A consumer's dry-run setting cannot alter the authenticated archive layout.
                for dry_run in ("0", "1"):
                    with patch.dict(
                        os.environ,
                        {
                            update_signing.DRY_RUN: dry_run,
                            update_signing.SIGNING_SECRET: base64.b64encode(
                                bytes(range(32))
                            ).decode(),
                        },
                    ):
                        if stapled == present:
                            core_package.verify(archive, signature, info, native=False)
                        else:
                            with self.assertRaisesRegex(TaskError, "supported layout"):
                                core_package.verify(archive, signature, info, native=False)

    def test_static_native_verification_never_executes_the_transferred_core(self):
        tree = self.root / "core"
        (tree / "bin").mkdir(parents=True)
        (tree / "bin/spaceterm").write_bytes(b"transferred executable")
        (tree / "build.json").write_text(json.dumps(self.info))
        populate(ROOT, tree, self.info)

        def inspect(arguments, message, **kwargs):
            if arguments[:2] == ["objdump", "-f"]:
                return b"elf64-x86-64"
            return "" if kwargs.get("text") else b""

        def process(arguments, **kwargs):
            if arguments[0] != "objdump":
                raise AssertionError("transferred product executed")
            return subprocess.CompletedProcess(arguments, 0, b"GLIBC_2.35", b"")

        with (
            patch.object(core_package, "sys", SimpleNamespace(platform="linux")),
            patch.object(core_package, "host_target", return_value="x86_64-unknown-linux-gnu"),
            patch.object(core_package, "checked", side_effect=inspect),
            patch.object(subprocess, "run", side_effect=process),
        ):
            core_package.verify_tree(tree, self.info, execute=False)

    def test_linux_cannot_produce_a_mac_cli_without_its_native_frameworks(self):
        with patch.object(core_package, "host_target", return_value="x86_64-unknown-linux-gnu"):
            with self.assertRaisesRegex(TaskError, "native macOS build"):
                core_package.package("spaceterm-development", target="aarch64-apple-darwin")

    def test_stapled_flag_is_required_boolean_and_only_valid_for_macos_release(self):
        for flag in (None, 0, 1, "false", [], True):
            with self.subTest(flag=flag):
                with self.assertRaisesRegex(TaskError, "malformed or unsupported"):
                    core_package.document(json.dumps({**self.info, "stapled": flag}))
        with self.assertRaisesRegex(TaskError, "malformed or unsupported"):
            core_package.document(
                json.dumps({key: value for key, value in self.info.items() if key != "stapled"})
            )
        with self.assertRaisesRegex(TaskError, "malformed or unsupported"):
            core_package.document(
                json.dumps({**self.info, "identity": "spaceterm", "stapled": True})
            )

    def test_package_records_the_actual_ticket_after_notarization(self):
        target = "aarch64-apple-darwin"
        info = {
            **self.info,
            "identity": "spaceterm",
            "target": target,
            "version": "1.0.0",
            "floor": "1.0.0",
            "source_sha256": "c" * 64,
        }
        executable = self.root / "compiled"
        executable.write_bytes(b"compiled executable")

        def icon(resources, identifier):
            for name in (*core_package.ICON_RESOURCES, "IconPartialInfo.plist"):
                (resources / name).write_bytes(b"compiled icon")

        def sign(bundle, *args, **kwargs):
            seal = bundle / "Contents/_CodeSignature/CodeResources"
            seal.parent.mkdir()
            seal.write_bytes(b"bundle seal")

        native_checked = core_package.checked

        def checked(arguments, *args, **kwargs):
            if arguments[0] == "git":
                return self.commit
            if arguments[0] == "lipo":
                return "arm64"
            if arguments[:2] == ["xcrun", "vtool"]:
                return "minos 26.0"
            if arguments[0] in ("otool", "infocmp"):
                return ""
            return native_checked(arguments, *args, **kwargs)

        run = subprocess.run

        def native_run(arguments, **kwargs):
            if arguments[1:] == ["internal", "build-info"]:
                return subprocess.CompletedProcess(arguments, 0, json.dumps(info).encode(), b"")
            if arguments[1:] == ["internal", "check"]:
                return subprocess.CompletedProcess(
                    arguments, 0, b"SpaceTerm Core checks passed.\n", b""
                )
            return run(arguments, **kwargs)

        for present in (True, False):

            def notarize(bundle, notary):
                if present:
                    (bundle / "Contents/CodeResources").write_bytes(b"stapled ticket")

            with (
                self.subTest(present=present),
                patch.dict(
                    os.environ,
                    {
                        update_signing.DRY_RUN: "1",
                        update_signing.SIGNING_SECRET: base64.b64encode(bytes(range(32))).decode(),
                        core_package.macos_codesign.TEAM_ID: "ABCDE12345",
                        core_package.macos_codesign.SIGNING_IDENTITY: "-",
                    },
                ),
                patch.object(core_package, "DIST", self.root / "dist"),
                patch.object(core_package, "host_target", return_value=target),
                patch.object(core_package, "sys", SimpleNamespace(platform="darwin")),
                patch.object(core_package, "require_clean_checkout"),
                patch("spaceterm_tasks.core_builds._source", return_value=(None, None, "c" * 64)),
                patch.object(
                    core_package, "build_executable", return_value=(executable, json.dumps(info))
                ),
                patch.object(core_package, "compile_icon", side_effect=icon),
                patch.object(core_package, "checked", side_effect=checked),
                patch.object(core_package.macos_codesign, "sign_core", side_effect=sign),
                patch.object(core_package.macos_codesign, "verify"),
                patch.object(core_package.macos_notary, "assess"),
                patch.object(core_package.macos_notary, "notarize", side_effect=notarize),
                patch.object(subprocess, "run", side_effect=native_run),
            ):
                archive, signature = core_package.package("spaceterm", "v1.0.0")
                with tarfile.open(archive) as source:
                    manifest = source.extractfile("core/build.json")
                    assert manifest is not None
                    packaged = json.load(manifest)
                self.assertEqual(packaged, {**info, "stapled": present})
                update_signing.verify((self.public,), archive, signature)

    def test_dirty_source_manifest_is_bound_to_its_commit_and_nonproduction_identity(self):
        for identity in ("spaceterm-development", "spaceterm-preflight", "spaceterm"):
            for commit in (self.commit, "b" * 40):
                info = {
                    **self.info,
                    "identity": identity,
                    "commit": commit,
                    "version": "dev.aaaaaaaaaaaa.dirty",
                }
                with self.subTest(identity=identity, commit=commit):
                    if identity == "spaceterm" or commit != self.commit:
                        with self.assertRaises(TaskError):
                            core_package.document(json.dumps(info))
                    else:
                        self.assertEqual(core_package.document(json.dumps(info)), info)

    def test_only_a_release_build_names_an_update_floor_at_or_below_its_version(self):
        release = {**self.info, "identity": "spaceterm", "version": "1.4.0"}
        for floor in ("1.4.0", "1.2.9", "0.9.0"):
            with self.subTest(floor=floor):
                info = {**release, "floor": floor}
                self.assertEqual(core_package.document(json.dumps(info)), info)
        for info in (
            {**release, "floor": None},
            {**release, "floor": "1.4.1"},
            {**release, "floor": "1.04.0"},
            {**self.info, "floor": "1.0.0"},
            {key: value for key, value in self.info.items() if key != "floor"},
        ):
            with self.subTest(info=info):
                with self.assertRaises(TaskError):
                    core_package.document(json.dumps(info))

    def archive_with(self, entries):
        with tarfile.open(self.archive, "w:gz") as archive:
            for name, payload, kind in entries:
                member = tarfile.TarInfo(name)
                member.type = kind
                member.size = len(payload) if kind == tarfile.REGTYPE else 0
                archive.addfile(member, io.BytesIO(payload))
        update_signing.sign(self.seed, self.archive, self.signature)

    def unpack(self):
        return core_package.unpack_verified(
            self.archive, self.signature, self.root / "unpacked", self.info
        )

    def test_signed_archive_retains_exact_candidate_identity(self):
        self.archive_with([("core/build.json", json.dumps(self.info).encode(), tarfile.REGTYPE)])
        self.assertEqual(self.unpack(), self.info)

    def test_nonproduction_artifact_trust_needs_no_production_secret_and_cannot_sign_release(self):
        trust = json.loads((ROOT / "packaging/core-nonproduction-trust.json").read_text())
        seed = base64.b64decode(trust["seed"])
        self.assertNotEqual(trust["public_key"], self.public)
        for identity in ("spaceterm-preflight", "spaceterm-development", "spaceterm"):
            with self.subTest(identity=identity):
                info = {**self.info, "identity": identity}
                self.archive_with([("core/build.json", json.dumps(info).encode(), tarfile.REGTYPE)])
                destination = self.root / identity
                environment = {"PATH": os.environ["PATH"]}
                if identity != "spaceterm":
                    environment["UPDATE_SIGNING_KEY"] = "invalid production credential"
                with patch.dict(os.environ, environment, clear=True):
                    signing_seed = (
                        seed
                        if identity == "spaceterm"
                        else core_package.artifact_signing_seed(identity)
                    )
                    update_signing.sign(signing_seed, self.archive, self.signature)
                    if identity == "spaceterm":
                        with self.assertRaises(TaskError):
                            core_package.unpack_verified(
                                self.archive, self.signature, destination, info
                            )
                        self.assertFalse(destination.exists())
                    else:
                        self.assertEqual(
                            core_package.unpack_verified(
                                self.archive, self.signature, destination, info
                            ),
                            info,
                        )

    def test_verified_extraction_does_not_leave_group_writable_implicit_directories(self):
        self.archive_with([("core/build.json", json.dumps(self.info).encode(), tarfile.REGTYPE)])
        previous = os.umask(0o002)
        try:
            self.unpack()
        finally:
            os.umask(previous)
        self.assertEqual(stat.S_IMODE((self.root / "unpacked/core").stat().st_mode), 0o755)

    def test_production_trust_cannot_authenticate_a_nonproduction_candidate(self):
        self.archive_with([("core/build.json", json.dumps(self.info).encode(), tarfile.REGTYPE)])
        production_fixture_seed = bytes(range(32))
        update_signing.sign(production_fixture_seed, self.archive, self.signature)
        with patch.object(
            update_signing,
            "release_trust",
            return_value=(
                update_signing.derived_public_key(bytes(range(1, 33))),
                update_signing.derived_public_key(production_fixture_seed),
            ),
        ):
            with self.assertRaises(TaskError):
                self.unpack()
        self.assertFalse((self.root / "unpacked").exists())

    def test_unsigned_and_tampered_archives_are_refused_before_extraction(self):
        self.archive_with([("core/build.json", json.dumps(self.info).encode(), tarfile.REGTYPE)])
        self.archive.write_bytes(self.archive.read_bytes() + b"tampered")
        with self.assertRaises(TaskError):
            self.unpack()
        self.assertFalse((self.root / "unpacked").exists())
        self.signature.unlink()
        with self.assertRaises((TaskError, OSError)):
            self.unpack()

    def test_signed_wrong_target_build_or_commit_is_refused(self):
        for key, value in [
            ("identity", "spaceterm"),
            ("target", "aarch64-apple-darwin"),
            ("commit", "b" * 40),
            ("version", "dev.other"),
        ]:
            with self.subTest(key=key):
                wrong = {**self.info, key: value}
                self.archive_with(
                    [("core/build.json", json.dumps(wrong).encode(), tarfile.REGTYPE)]
                )
                with self.assertRaises(TaskError):
                    self.unpack()

    def test_signed_unsafe_or_ambiguous_layout_is_refused(self):
        for name, kind in [
            ("../escaped", tarfile.REGTYPE),
            ("/absolute", tarfile.REGTYPE),
            ("core/link", tarfile.SYMTYPE),
            ("core/device", tarfile.CHRTYPE),
            ("core//duplicate", tarfile.REGTYPE),
            ("other/file", tarfile.REGTYPE),
        ]:
            with self.subTest(name=name):
                self.archive_with([(name, b"payload", kind)])
                with self.assertRaises(TaskError):
                    self.unpack()
        entry = ("core/build.json", json.dumps(self.info).encode(), tarfile.REGTYPE)
        self.archive_with([entry, entry])
        with self.assertRaises(TaskError):
            self.unpack()

    def test_signed_package_with_missing_or_extra_resources_is_refused(self):
        tree = self.root / "core"
        (tree / "bin").mkdir(parents=True)
        (tree / "bin/spaceterm").write_bytes(b"never executed")
        (tree / "bin/spaceterm").chmod(0o755)
        (tree / "build.json").write_text(json.dumps(self.info))
        resources = tree / "share/spaceterm"
        shutil.copytree(SHELL_INTEGRATION, resources / "shell-integration")
        shutil.copyfile(NOTICES, resources / NOTICES.name)
        subprocess.run(
            ["tic", "-x", "-o", str(resources / "terminfo"), str(TERMINFO)],
            check=True,
            capture_output=True,
        )
        missing = resources / "shell-integration/zsh/.zshenv"
        saved = missing.read_bytes()
        missing.unlink()
        for extra in (False, True):
            if extra:
                missing.write_bytes(saved)
                (resources / "unexpected").mkdir()
            write_archive(tree, self.archive)
            update_signing.sign(self.seed, self.archive, self.signature)
            with tempfile.TemporaryDirectory(dir=self.root) as temporary:
                with self.assertRaisesRegex(TaskError, "supported layout"):
                    core_package.verified_copy(
                        self.archive, self.signature, Path(temporary), self.info
                    )

    def test_desktop_artifact_rejects_wrong_exact_build_before_staging(self):
        tree = self.root / "core"
        (tree / "bin").mkdir(parents=True)
        (tree / "bin/spaceterm").write_bytes(b"native executable fixture")
        (tree / "bin/spaceterm").chmod(0o755)
        populate(ROOT, tree, self.info)
        archive, signature = archive_tree(ROOT, tree, self.info)
        run = subprocess.run
        client = self.root / "client"
        client_pin = {"value": update_signing.sha256(archive)}
        core_queries = []

        def native_run(arguments, **kwargs):
            if arguments[0] == client:
                output = json.dumps(
                    {
                        "schema": 1,
                        "source_sha256": None,
                        "identity": self.info["identity"],
                        "target": self.info["target"],
                        "version": "99.0.0",
                        "core_candidate_sha256": client_pin["value"],
                    }
                ).encode()
            elif arguments[0] == "objdump":
                output = b"elf64-x86-64" if arguments[1] == "-f" else b""
            elif arguments[1:] == ["internal", "build-info"]:
                core_queries.append(arguments[0])
                output = json.dumps(self.info).encode()
            elif arguments[1:] == ["internal", "check"]:
                output = b"SpaceTerm Core checks passed.\n"
            else:
                return run(arguments, **kwargs)
            if kwargs.get("text"):
                output = output.decode()
            return subprocess.CompletedProcess(arguments, 0, stdout=output, stderr=b"")

        with (
            patch.object(core_package, "sys", SimpleNamespace(platform="linux")),
            patch.object(core_package, "host_target", return_value=self.info["target"]),
            patch.object(subprocess, "run", side_effect=native_run),
        ):
            baseline = self.root / "desktop/baseline"
            core_package.stage_desktop_artifact(archive, signature, baseline, self.info)
            self.assertEqual((baseline / "artifact.tar.gz").read_bytes(), archive.read_bytes())
            self.assertEqual((baseline / "artifact.sig").read_bytes(), signature.read_bytes())
            # The client release is independent; its compiled pin owns the exact candidate.
            core_package.verify_desktop_artifact(
                baseline, self.info["identity"], "v99.0.0", executable=client
            )
            with (
                patch.dict(
                    os.environ,
                    {
                        "PATH": os.environ["PATH"],
                        "SPACETERM_CLI_ARCHIVE": str(archive),
                        "SPACETERM_CLI_SIGNATURE": str(signature),
                    },
                    clear=True,
                ),
                patch.object(core_package, "require_clean_checkout"),
                patch.object(
                    core_package, "package", side_effect=AssertionError("must reuse signed bytes")
                ),
            ):
                # A release embeds only the Core builds release:core produced.
                with self.assertRaisesRegex(TaskError, "SPACETERM_CLI_ARTIFACTS"):
                    core_package.prepare_desktop_artifact(
                        self.root / "desktop/release", self.info["identity"], "v99.0.0"
                    )
                reused = self.root / "desktop/reused"
                selected = core_package.prepare_desktop_artifact(reused, self.info["identity"])
                self.assertEqual(
                    Path(selected["SPACETERM_CLI_ARCHIVE"]).read_bytes(), archive.read_bytes()
                )
                core_package.verify_desktop_artifact(
                    reused, self.info["identity"], executable=client
                )
            verified_queries = len(core_queries)
            client_pin["value"] = "0" * 64
            with self.assertRaises(core_package.ArtifactVerificationError) as failure:
                core_package.verify_desktop_artifact(
                    baseline, self.info["identity"], executable=client
                )
            self.assertEqual(failure.exception.cause, core_package.ArtifactCause.DIGEST_MISMATCH)
            self.assertEqual(
                str(failure.exception),
                "SpaceTerm Core artifact verification failed: digest mismatch",
            )
            self.assertEqual(len(core_queries), verified_queries)
            destination = self.root / "desktop/wrong-build"
            expected = {**self.info, "commit": "b" * 40, "version": "dev.bbbbbbbbbbbb"}
            with self.assertRaisesRegex(
                TaskError, "artifact differs from the requested exact build"
            ):
                core_package.stage_desktop_artifact(archive, signature, destination, expected)
        self.assertFalse(destination.exists())

    def test_candidate_compatibility_requires_the_protocol_major(self):
        with patch.object(core_package, "host_target", return_value=self.info["target"]):
            core_package.compatible_candidate(self.info, self.info["identity"])
        with (
            patch.object(core_package, "host_target", return_value=self.info["target"]),
            self.assertRaisesRegex(TaskError, "protocol major"),
        ):
            core_package.compatible_candidate(
                {
                    **self.info,
                    "protocols": {
                        **core_package.PROTOCOLS,
                        **{
                            "work": {
                                **core_package.PROTOCOLS["work"],
                                "major": core_package.PROTOCOL_MAJOR + 1,
                            }
                        },
                    },
                },
                self.info["identity"],
            )

    def test_desktop_candidate_requires_both_explicit_artifact_inputs(self):
        with patch.dict(os.environ, {"SPACETERM_CLI_ARCHIVE": str(self.archive)}, clear=True):
            with self.assertRaisesRegex(TaskError, "both"):
                core_package.prepare_desktop_artifact(
                    self.root / "desktop/core-artifact", "spaceterm-preflight"
                )

    def test_signed_malformed_build_document_is_refused(self):
        for payload in [b"null", b"[]", b"{}", b"invalid", b'{"schema":1,"schema":2}']:
            self.archive_with([("core/build.json", payload, tarfile.REGTYPE)])
            with self.assertRaises(TaskError):
                self.unpack()


class DesktopArtifactFailureTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.destination = Path(temporary.name) / "private-artifact"
        self.destination.mkdir()
        self.archive = self.destination / "artifact.tar.gz"
        self.signature = self.destination / "artifact.sig"
        self.archive.write_bytes(b"private archive contents")
        self.signature.write_bytes(bytes(64))

    def assert_failure(self, cause, message, *, executable=None):
        with self.assertRaises(core_package.ArtifactVerificationError) as failure:
            core_package.verify_desktop_artifact(
                self.destination, "spaceterm-preflight", executable=executable
            )
        self.assertEqual(failure.exception.cause, cause)
        self.assertEqual(str(failure.exception), message)
        self.assertTrue(failure.exception.__suppress_context__)

    def test_missing_directory_or_artifact_is_classified(self):
        for name in ("artifact.tar.gz", "artifact.sig"):
            with self.subTest(name=name):
                path = self.destination / name
                contents = path.read_bytes()
                path.unlink()
                self.assert_failure(
                    core_package.ArtifactCause.MISSING,
                    "SpaceTerm Core artifact verification failed: missing",
                )
                path.write_bytes(contents)
        self.archive.unlink()
        self.signature.unlink()
        self.destination.rmdir()
        self.assert_failure(
            core_package.ArtifactCause.MISSING,
            "SpaceTerm Core artifact verification failed: missing",
        )

    def test_unreadable_artifact_is_classified(self):
        self.archive.chmod(0)
        self.addCleanup(self.archive.chmod, 0o600)
        self.assert_failure(
            core_package.ArtifactCause.UNREADABLE,
            "SpaceTerm Core artifact verification failed: unreadable",
        )

    def test_client_query_timeout_discards_command_and_output(self):
        with patch.object(
            subprocess,
            "run",
            side_effect=subprocess.TimeoutExpired(
                ["/private/client", "--build-info"],
                10,
                output=b"private stdout",
                stderr=b"private stderr",
            ),
        ):
            self.assert_failure(
                core_package.ArtifactCause.TIMED_OUT,
                "SpaceTerm Core artifact verification failed: timed out",
                executable=Path("/private/client"),
            )

    def test_signature_mismatch_is_classified(self):
        self.assert_failure(
            core_package.ArtifactCause.SIGNATURE_MISMATCH,
            "SpaceTerm Core artifact verification failed: signature mismatch",
        )

    def test_other_os_failure_discards_native_details(self):
        self.archive.unlink()
        self.signature.unlink()
        self.destination.rmdir()
        self.destination.write_text("private native error path")
        self.assert_failure(
            core_package.ArtifactCause.OS_ERROR,
            "SpaceTerm Core artifact verification failed: operating-system operation failed",
        )


class CrossBuildTests(unittest.TestCase):
    def test_linux_target_terminfo_has_the_ncurses_character_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "terminfo"
            core_package.compile_terminfo(destination, "x86_64-unknown-linux-gnu")
            entry = destination / "x/xterm-spaceterm"
            core_package.verify_foreign_terminfo(entry)
            hexadecimal = destination / "78/xterm-spaceterm"
            if hexadecimal.exists():
                self.assertEqual(entry.read_bytes(), hexadecimal.read_bytes())

    def test_foreign_release_and_unknown_target_are_rejected_before_build(self):
        # A release, not a dry run, whatever release mode the caller runs in.
        environment = {k: v for k, v in os.environ.items() if k != update_signing.DRY_RUN}
        with (
            patch.dict(os.environ, environment, clear=True),
            patch.object(core_package, "host_target", return_value="aarch64-apple-darwin"),
        ):
            with self.assertRaisesRegex(TaskError, "native target host"):
                core_package.package("spaceterm", "v1.0.0", target="x86_64-unknown-linux-gnu")
            with self.assertRaisesRegex(TaskError, "unsupported"):
                core_package.package("spaceterm-preflight", target="other")

    def test_mac_target_terminfo_is_legacy_and_preserves_extended_capabilities(self):
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "terminfo"
            with patch.object(core_package, "sys", SimpleNamespace(platform="linux")):
                core_package.compile_terminfo(destination, "aarch64-apple-darwin")
            entry = destination / "78/xterm-spaceterm"
            self.assertEqual(entry.read_bytes()[:2], bytes.fromhex("1a01"))
            # Decode the macOS bytes through the running host's terminfo lookup layout.
            native = Path(temporary) / "native-terminfo"
            lookup = native / ("78" if os.uname().sysname == "Darwin" else "x")
            lookup.mkdir(parents=True)
            shutil.copyfile(entry, lookup / entry.name)
            result = subprocess.run(
                ["infocmp", "-x", "-1", "xterm-spaceterm"],
                env={**os.environ, "TERMINFO": str(native)},
                capture_output=True,
                text=True,
                check=True,
            ).stdout
            for name, expected in (("colors", 256), ("pairs", 32767), ("RGB", 8)):
                match = re.search(rf"(?m)^\s*{name}#(0x[0-9a-fA-F]+|[0-9]+),", result)
                self.assertIsNotNone(match)
                assert match is not None
                self.assertEqual(int(match[1], 0), expected)
            for capability in ("Tc,", "Ms=", "Ss=", "Se="):
                self.assertIn(capability, result)


if __name__ == "__main__":
    unittest.main()


class TarExpansionTests(unittest.TestCase):
    def test_signed_gnu_and_pax_bodies_are_bounded_before_library_allocation(self):
        import gzip

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / "bomb.tar.gz"
            signature = root / "bomb.sig"
            seed = core_package.artifact_signing_seed("spaceterm-preflight")
            for kind in (tarfile.GNUTYPE_LONGNAME, tarfile.XHDTYPE):
                header = tarfile.TarInfo("././@LongLink")
                header.type = kind
                header.size = 537_919_488
                with gzip.open(archive, "wb") as output:
                    output.write(header.tobuf(format=tarfile.GNU_FORMAT))
                    output.write(bytes(1024))
                update_signing.sign(seed, archive, signature)
                original = gzip.GzipFile.read
                consumed = 0

                def guarded(stream, size=-1):
                    nonlocal consumed
                    if size < 0 or size > 64 * 1024 or consumed + size > 64 * 1024:
                        self.fail("tar metadata triggered unbounded decompression")
                    block = original(stream, size)
                    consumed += len(block)
                    return block

                with patch.object(gzip.GzipFile, "read", guarded):
                    with self.assertRaisesRegex(TaskError, "extension exceeds"):
                        core_package.unpack_verified(
                            archive,
                            signature,
                            root / "output",
                            None,
                            identity="spaceterm-preflight",
                        )
                self.assertLessEqual(consumed, 512)

    def test_total_padding_and_physical_extension_counts_are_bounded(self):
        import gzip

        from spaceterm_tasks.bounded_tar import open_archive

        with tempfile.TemporaryDirectory() as temporary:
            archive = Path(temporary) / "records.tar.gz"
            header = tarfile.TarInfo("pax")
            header.type = tarfile.XHDTYPE
            header.size = 0
            with gzip.open(archive, "wb") as output:
                output.write(header.tobuf(format=tarfile.PAX_FORMAT) * 5)
            with self.assertRaisesRegex(TaskError, "physical records"):
                with open_archive(archive, maximum=8192, members=2):
                    self.fail("too many extensions accepted")
            with gzip.open(archive, "wb") as output:
                output.write(bytes(8192))
            with self.assertRaisesRegex(TaskError, "expanded size"):
                with open_archive(archive, maximum=4096, members=2):
                    self.fail("excess trailing expansion accepted")
