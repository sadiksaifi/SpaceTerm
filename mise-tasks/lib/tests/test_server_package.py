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

from spaceterm_tasks import ROOT, TaskError, release, server_package
from spaceterm_tasks.package_linux import write_archive
from spaceterm_tasks.packaging import NOTICES, SHELL_INTEGRATION, TERMINFO

from tests.server_artifact_fixture import archive_tree, populate


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
            json.loads((ROOT / "packaging/server-resources.json").read_text()),
            ["share/spaceterm/" + name.removeprefix("assets/") for name in tracked],
        )


class ServerArchiveTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.seed = server_package.artifact_signing_seed("spaceterm-preflight")
        self.public = release.public_key()
        self.archive = self.root / "server.tar.gz"
        self.signature = self.root / "server.tar.gz.sig"
        self.commit = "a" * 40
        self.info = {
            "schema": 1,
            "source_sha256": None,
            "protocol_major": server_package.PROTOCOL_MAJOR,
            "identity": "spaceterm-preflight",
            "target": "x86_64-unknown-linux-gnu",
            "commit": self.commit,
            "version": "dev.aaaaaaaaaaaa",
        }

    def test_linux_cannot_produce_a_mac_cli_without_its_native_frameworks(self):
        with patch.object(server_package, "host_target", return_value="x86_64-unknown-linux-gnu"):
            with self.assertRaisesRegex(TaskError, "native macOS build"):
                server_package.package("spaceterm-development", target="aarch64-apple-darwin")

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
                            server_package.document(json.dumps(info))
                    else:
                        self.assertEqual(server_package.document(json.dumps(info)), info)

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

    def test_nonproduction_artifact_trust_needs_no_production_secret_and_cannot_sign_release(self):
        trust = json.loads((ROOT / "packaging/server-nonproduction-trust.json").read_text())
        seed = base64.b64decode(trust["seed"])
        self.assertNotEqual(trust["public_key"], self.public)
        for identity in ("spaceterm-preflight", "spaceterm-development", "spaceterm"):
            with self.subTest(identity=identity):
                info = {**self.info, "identity": identity}
                self.archive_with(
                    [("server/build.json", json.dumps(info).encode(), tarfile.REGTYPE)]
                )
                destination = self.root / identity
                environment = {"PATH": os.environ["PATH"]}
                if identity != "spaceterm":
                    environment["UPDATE_SIGNING_KEY"] = "invalid production credential"
                with patch.dict(os.environ, environment, clear=True):
                    signing_seed = (
                        seed
                        if identity == "spaceterm"
                        else server_package.artifact_signing_seed(identity)
                    )
                    release.sign_ed25519(signing_seed, self.archive, self.signature)
                    if identity == "spaceterm":
                        with self.assertRaises(TaskError):
                            server_package.unpack_verified(
                                self.archive, self.signature, destination, info
                            )
                        self.assertFalse(destination.exists())
                    else:
                        self.assertEqual(
                            server_package.unpack_verified(
                                self.archive, self.signature, destination, info
                            ),
                            info,
                        )

    def test_verified_extraction_does_not_leave_group_writable_implicit_directories(self):
        self.archive_with([("server/build.json", json.dumps(self.info).encode(), tarfile.REGTYPE)])
        previous = os.umask(0o002)
        try:
            self.unpack()
        finally:
            os.umask(previous)
        self.assertEqual(stat.S_IMODE((self.root / "unpacked/server").stat().st_mode), 0o755)

    def test_production_trust_cannot_authenticate_a_nonproduction_candidate(self):
        self.archive_with([("server/build.json", json.dumps(self.info).encode(), tarfile.REGTYPE)])
        production_fixture_seed = bytes(range(32))
        release.sign_ed25519(production_fixture_seed, self.archive, self.signature)
        with patch.object(
            release, "public_key", return_value=release.derived_public_key(production_fixture_seed)
        ):
            with self.assertRaises(TaskError):
                self.unpack()
        self.assertFalse((self.root / "unpacked").exists())

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
            release.sign_ed25519(self.seed, self.archive, self.signature)
            with tempfile.TemporaryDirectory(dir=self.root) as temporary:
                with self.assertRaisesRegex(TaskError, "supported layout"):
                    server_package.verified_copy(
                        self.archive, self.signature, Path(temporary), self.info
                    )

    def test_desktop_artifact_rejects_wrong_exact_build_before_staging(self):
        tree = self.root / "server"
        (tree / "bin").mkdir(parents=True)
        (tree / "bin/spaceterm").write_bytes(b"native executable fixture")
        (tree / "bin/spaceterm").chmod(0o755)
        populate(ROOT, tree, self.info)
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
                        "source_sha256": None,
                        "identity": self.info["identity"],
                        "target": self.info["target"],
                        "version": "99.0.0",
                        "server_candidate_sha256": client_pin["value"],
                    }
                ).encode()
            elif arguments[0] == "objdump":
                output = b"elf64-x86-64" if arguments[1] == "-f" else b""
            elif arguments[1:] == ["server", "provision", "build-info"]:
                server_queries.append(arguments[0])
                output = json.dumps(self.info).encode()
            elif arguments[1:] == ["server", "provision", "check"]:
                output = b"Server checks passed.\n"
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

    def test_candidate_compatibility_requires_the_protocol_major(self):
        with patch.object(server_package, "host_target", return_value=self.info["target"]):
            server_package.compatible_candidate(self.info, self.info["identity"])
        with (
            patch.object(server_package, "host_target", return_value=self.info["target"]),
            self.assertRaisesRegex(TaskError, "protocol major"),
        ):
            server_package.compatible_candidate(
                {**self.info, "protocol_major": server_package.PROTOCOL_MAJOR + 1},
                self.info["identity"],
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


class CrossBuildTests(unittest.TestCase):
    def test_linux_target_terminfo_has_the_ncurses_character_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "terminfo"
            server_package.compile_terminfo(destination, "x86_64-unknown-linux-gnu")
            entry = destination / "x/xterm-spaceterm"
            server_package.verify_foreign_terminfo(entry)
            hexadecimal = destination / "78/xterm-spaceterm"
            if hexadecimal.exists():
                self.assertEqual(entry.read_bytes(), hexadecimal.read_bytes())

    def test_foreign_release_and_unknown_target_are_rejected_before_build(self):
        with patch.object(server_package, "host_target", return_value="aarch64-apple-darwin"):
            with self.assertRaisesRegex(TaskError, "native target host"):
                server_package.package("spaceterm", "v1.0.0", target="x86_64-unknown-linux-gnu")
            with self.assertRaisesRegex(TaskError, "unsupported"):
                server_package.package("spaceterm-preflight", target="other")

    def test_mac_target_terminfo_is_legacy_and_preserves_extended_capabilities(self):
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "terminfo"
            with patch.object(server_package, "sys", SimpleNamespace(platform="linux")):
                server_package.compile_terminfo(destination, "aarch64-apple-darwin")
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


class MachoCrossBuildTests(unittest.TestCase):
    def test_macos_load_commands_require_exact_deployment_and_safe_dependencies(self):
        for minimum, dependency, valid in (
            ("26.0", "/usr/lib/libSystem.B.dylib", True),
            ("13.0", "/usr/lib/libSystem.B.dylib", False),
            ("26.0", "/System/Library/Frameworks/Metal.framework/Metal", False),
            ("26.0", "@rpath/foreign.dylib", False),
        ):
            output = f"""Mach header
      magic cputype cpusubtype caps filetype ncmds sizeofcmds flags
MH_MAGIC_64 ARM64 ALL 0x00 EXECUTE 2 80 NOUNDEFS DYLDLINK TWOLEVEL PIE
Load command 0
       cmd LC_BUILD_VERSION
   cmdsize 24
  platform macos
       sdk 26.0
     minos {minimum}
    ntools 0
Load command 1
          cmd LC_LOAD_DYLIB
      cmdsize 56
         name {dependency} (offset 24)
   time stamp 2 Thu Jan 1 1970
      current version 1.0.0
compatibility version 1.0.0
"""
            with self.subTest(minimum=minimum, dependency=dependency):
                if valid:
                    server_package.verify_macho_output(output)
                    with self.assertRaises(TaskError):
                        server_package.verify_macho_output(
                            output.replace("Load command 1", "Load command 2")
                        )
                else:
                    with self.assertRaises(TaskError):
                        server_package.verify_macho_output(output)

    def test_rcodesign_identity_requires_exact_identifier_and_adhoc_flags(self):
        identifier = "io.github.sadiksaifi.spaceterm-preflight.server"
        output = f'''CodeDirectoryBlob {{
    flags: CodeSignatureFlags(
        ADHOC,
    ),
    ident: "{identifier}",
}}
'''
        server_package.verify_macho_identity(output, identifier)
        for changed in (
            output.replace(identifier, "foreign"),
            output.replace("ADHOC", "RUNTIME"),
            output.replace("    ident:", '    ident: "duplicate",\n    ident:'),
            "",
        ):
            with self.subTest(changed=changed):
                with self.assertRaises(TaskError):
                    server_package.verify_macho_identity(changed, identifier)


if __name__ == "__main__":
    unittest.main()
