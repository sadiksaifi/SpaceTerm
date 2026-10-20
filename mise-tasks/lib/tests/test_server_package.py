"""Signed server archives are authenticated before layout or executable inspection."""

import io
import json
import os
import shutil
import stat
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from spaceterm_tasks import ROOT, TaskError, release, server_package
from spaceterm_tasks.package_linux import write_archive
from spaceterm_tasks.packaging import NOTICES, SHELL_INTEGRATION, TERMINFO

from tests.server_artifact_fixture import archive_tree


class ServerArchiveTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.seed = bytes(range(32))
        self.public = release.derived_public_key(self.seed)
        self.archive = self.root / "server.tar.gz"
        self.signature = self.root / "server.tar.gz.sig"
        self.commit = "a" * 40
        self.info = {
            "schema": 1,
            "protocol_major": 1,
            "capabilities": ["workspace_policy"],
            "identity": "spaceterm-preflight",
            "target": "x86_64-unknown-linux-gnu",
            "commit": self.commit,
            "version": "dev.aaaaaaaaaaaa",
        }
        trust = patch.object(release, "public_key", return_value=self.public)
        trust.start()
        self.addCleanup(trust.stop)

    def archive_with(self, entries):
        with tarfile.open(self.archive, "w:gz") as archive:
            for name, payload, kind in entries:
                member = tarfile.TarInfo(name)
                member.type = kind
                member.size = len(payload) if kind == tarfile.REGTYPE else 0
                archive.addfile(member, io.BytesIO(payload))
        release.sign_ed25519(self.seed, self.archive, self.signature)

    def unpack(self):
        return server_package.unpack_verified(
            self.archive, self.signature, self.root / "unpacked", self.info
        )

    def test_signed_archive_retains_exact_candidate_identity(self):
        self.archive_with([("server/build.json", json.dumps(self.info).encode(), tarfile.REGTYPE)])
        self.assertEqual(self.unpack(), self.info)

    def test_verified_extraction_does_not_leave_group_writable_implicit_directories(self):
        self.archive_with([("server/build.json", json.dumps(self.info).encode(), tarfile.REGTYPE)])
        previous = os.umask(0o002)
        try:
            self.unpack()
        finally:
            os.umask(previous)
        self.assertEqual(stat.S_IMODE((self.root / "unpacked/server").stat().st_mode), 0o755)

    def test_unsigned_and_tampered_archives_are_refused_before_extraction(self):
        self.archive_with([("server/build.json", json.dumps(self.info).encode(), tarfile.REGTYPE)])
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
                    [("server/build.json", json.dumps(wrong).encode(), tarfile.REGTYPE)]
                )
                with self.assertRaises(TaskError):
                    self.unpack()

    def test_signed_unsafe_or_ambiguous_layout_is_refused(self):
        for name, kind in [
            ("../escaped", tarfile.REGTYPE),
            ("/absolute", tarfile.REGTYPE),
            ("server/link", tarfile.SYMTYPE),
            ("server/device", tarfile.CHRTYPE),
            ("server//duplicate", tarfile.REGTYPE),
            ("other/file", tarfile.REGTYPE),
        ]:
            with self.subTest(name=name):
                self.archive_with([(name, b"payload", kind)])
                with self.assertRaises(TaskError):
                    self.unpack()
        entry = ("server/build.json", json.dumps(self.info).encode(), tarfile.REGTYPE)
        self.archive_with([entry, entry])
        with self.assertRaises(TaskError):
            self.unpack()

    def test_signed_package_with_missing_or_extra_resources_is_refused(self):
        tree = self.root / "server"
        (tree / "bin").mkdir(parents=True)
        (tree / "bin/spaceterm-server").write_bytes(b"never executed")
        (tree / "bin/spaceterm-server").chmod(0o755)
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
            release.sign_ed25519(self.seed, self.archive, self.signature)
            with tempfile.TemporaryDirectory(dir=self.root) as temporary:
                with self.assertRaisesRegex(TaskError, "supported layout"):
                    server_package.verified_copy(
                        self.archive, self.signature, Path(temporary), self.info
                    )

    def test_desktop_artifact_rejects_wrong_exact_build_before_staging(self):
        tree = self.root / "server"
        (tree / "bin").mkdir(parents=True)
        (tree / "bin/spaceterm-server").write_bytes(b"native executable fixture")
        (tree / "bin/spaceterm-server").chmod(0o755)
        archive, signature = archive_tree(ROOT, tree, self.info)
        run = subprocess.run
        client = self.root / "client"
        client_pin = {"value": release.sha256(archive)}
        server_queries = []

        def native_run(arguments, **kwargs):
            if arguments[0] == client:
                output = json.dumps(
                    {
                        "schema": 1,
                        "identity": self.info["identity"],
                        "target": self.info["target"],
                        "version": "99.0.0",
                        "server_candidate_sha256": client_pin["value"],
                    }
                ).encode()
            elif arguments[0] == "objdump":
                output = b"elf64-x86-64" if arguments[1] == "-f" else b""
            elif arguments[1:] == ["--build-info"]:
                server_queries.append(arguments[0])
                output = json.dumps(self.info).encode()
            elif arguments[1:] == ["--check"]:
                output = b"Installed server PTY and shell resources verified.\n"
            else:
                return run(arguments, **kwargs)
            if kwargs.get("text"):
                output = output.decode()
            return subprocess.CompletedProcess(arguments, 0, stdout=output, stderr=b"")

        with (
            patch.object(server_package, "sys", SimpleNamespace(platform="linux")),
            patch.object(server_package, "host_target", return_value=self.info["target"]),
            patch.object(subprocess, "run", side_effect=native_run),
        ):
            baseline = self.root / "desktop/baseline"
            server_package.stage_desktop_artifact(archive, signature, baseline, self.info)
            self.assertEqual((baseline / "artifact.tar.gz").read_bytes(), archive.read_bytes())
            self.assertEqual((baseline / "artifact.sig").read_bytes(), signature.read_bytes())
            # The client release is independent; its compiled pin owns the exact candidate.
            server_package.verify_desktop_artifact(
                baseline, self.info["identity"], "v99.0.0", executable=client
            )
            with (
                patch.dict(
                    os.environ,
                    {
                        "PATH": os.environ["PATH"],
                        "SPACETERM_SERVER_ARCHIVE": str(archive),
                        "SPACETERM_SERVER_SIGNATURE": str(signature),
                    },
                    clear=True,
                ),
                patch.object(server_package, "require_clean_checkout"),
                patch.object(
                    server_package, "package", side_effect=AssertionError("must reuse signed bytes")
                ),
            ):
                reused = self.root / "desktop/reused"
                selected = server_package.prepare_desktop_artifact(
                    reused, self.info["identity"], "v99.0.0"
                )
                self.assertEqual(
                    Path(selected["SPACETERM_SERVER_ARCHIVE"]).read_bytes(), archive.read_bytes()
                )
                server_package.verify_desktop_artifact(
                    reused, self.info["identity"], executable=client
                )
            verified_queries = len(server_queries)
            client_pin["value"] = "0" * 64
            with self.assertRaisesRegex(TaskError, "compiled candidate pin"):
                server_package.verify_desktop_artifact(
                    baseline, self.info["identity"], executable=client
                )
            self.assertEqual(len(server_queries), verified_queries)
            destination = self.root / "desktop/wrong-build"
            expected = {**self.info, "commit": "b" * 40, "version": "dev.bbbbbbbbbbbb"}
            with self.assertRaisesRegex(
                TaskError, "artifact differs from the requested exact build"
            ):
                server_package.stage_desktop_artifact(archive, signature, destination, expected)
        self.assertFalse(destination.exists())

    def test_candidate_compatibility_requires_major_and_required_capability_only(self):
        with patch.object(server_package, "host_target", return_value=self.info["target"]):
            server_package.compatible_candidate(self.info, self.info["identity"])
        for changed, reason in [
            ({"protocol_major": 2}, "protocol major"),
            ({"capabilities": ["future_optional"]}, "required capability"),
        ]:
            with self.subTest(changed=changed):
                with (
                    patch.object(server_package, "host_target", return_value=self.info["target"]),
                    self.assertRaisesRegex(TaskError, reason),
                ):
                    server_package.compatible_candidate(
                        {**self.info, **changed}, self.info["identity"]
                    )

    def test_desktop_candidate_requires_both_explicit_artifact_inputs(self):
        with patch.dict(os.environ, {"SPACETERM_SERVER_ARCHIVE": str(self.archive)}, clear=True):
            with self.assertRaisesRegex(TaskError, "both"):
                server_package.prepare_desktop_artifact(
                    self.root / "desktop/server-artifact", "spaceterm-preflight"
                )

    def test_missing_desktop_artifact_is_classified_without_a_native_error(self):
        with self.assertRaisesRegex(TaskError, "desktop server artifact is unavailable"):
            server_package.verify_desktop_artifact(self.root / "missing", "spaceterm-preflight")

    def test_signed_malformed_build_document_is_refused(self):
        for payload in [b"null", b"[]", b"{}", b"invalid", b'{"schema":1,"schema":2}']:
            self.archive_with([("server/build.json", payload, tarfile.REGTYPE)])
            with self.assertRaises(TaskError):
                self.unpack()


if __name__ == "__main__":
    unittest.main()
