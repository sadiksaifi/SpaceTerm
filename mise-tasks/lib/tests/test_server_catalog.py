"""A packaged SSH catalog retains only authenticated target-specific server bytes."""

import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import ROOT, TaskError, macos_signing, release, server_package
from spaceterm_tasks.package_linux import write_archive
from spaceterm_tasks.server_catalog import stage_catalog, verify_catalog

from tests.macos_signing_fixture import certificate
from tests.server_artifact_fixture import SEED, archive_tree


class ServerCatalogTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if sys.platform == "darwin":
            temporary = tempfile.TemporaryDirectory()
            cls.addClassCleanup(temporary.cleanup)
            cls.fingerprint, cls.credentials = certificate(Path(temporary.name))
            signer = patch.object(server_package, "signer", return_value=cls.fingerprint)
            signer.start()
            cls.addClassCleanup(signer.stop)

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        trust = patch.object(release, "public_key", return_value=release.derived_public_key(SEED))
        trust.start()
        self.addCleanup(trust.stop)
        self.identity = "spaceterm-preflight"
        self.target = server_package.host_target()

    def artifact(self, native_target, **changed):
        directory = Path(tempfile.mkdtemp(dir=self.root))
        tree = directory / "server"
        executable = tree / "bin/spaceterm-server"
        executable.parent.mkdir(parents=True)
        # Signed foreign executables are opaque to the catalog. These headers declare
        # the supported machine; native producers separately verify their executables.
        if native_target == "aarch64-apple-darwin":
            header = bytes.fromhex("cffaedfe0c0000010000000002000000") + bytes(16)
        else:
            header = b"\x7fELF\x02\x01\x01" + bytes(9) + bytes.fromhex("03003e00") + bytes(44)
        info = dict(
            schema=1,
            identity=self.identity,
            target=native_target,
            commit="a" * 40,
            version="dev.aaaaaaaaaaaa",
            protocol_major=1,
            capabilities=["workspace_policy"],
        )
        info.update(changed)
        if native_target == self.target:
            source = f"""
#include <stdio.h>
#include <string.h>
int main(int argc, char **argv) {{
    if (argc == 2 && strcmp(argv[1], "--build-info") == 0) {{
        puts({json.dumps(json.dumps(info))});
        return 0;
    }}
    if (argc == 2 && strcmp(argv[1], "--check") == 0) {{
        puts("Installed server PTY and shell resources verified.");
        return 0;
    }}
    return 1;
}}
"""
            compiler = (
                ["xcrun", "clang", "-arch", "arm64", "-mmacosx-version-min=26.0"]
                if sys.platform == "darwin"
                else ["cc"]
            )
            subprocess.run(
                [*compiler, "-x", "c", "-", "-o", str(executable)],
                input=source,
                text=True,
                capture_output=True,
                check=True,
            )
            if sys.platform == "darwin":
                identifier = f"io.github.sadiksaifi.{info['identity']}.server"
                requirement = f'=designated => identifier "{identifier}" and {macos_signing.certificate_requirement(self.fingerprint)}'
                with macos_signing.release_keychain(self.fingerprint, self.credentials):
                    subprocess.run(
                        [
                            "codesign",
                            "--force",
                            "--sign",
                            self.fingerprint,
                            "--identifier",
                            identifier,
                            "--requirements",
                            requirement,
                            "--timestamp=none",
                            str(executable),
                        ],
                        check=True,
                        capture_output=True,
                    )
        else:
            executable.write_bytes(header)
            executable.chmod(0o755)
        return (*archive_tree(ROOT, tree, info), info)

    def test_default_catalog_retains_the_exact_local_pair_and_complete_digest(self):
        archive, signature, info = self.artifact(self.target)
        destination = self.root / "catalog"
        stage_catalog(archive, signature, destination, self.identity)
        target = destination / self.target
        self.assertEqual((target / "artifact.tar.gz").read_bytes(), archive.read_bytes())
        self.assertEqual((target / "artifact.sig").read_bytes(), signature.read_bytes())
        self.assertEqual((target / "artifact.sha256").read_text(), release.sha256(archive) + "\n")
        self.assertEqual(verify_catalog(destination, self.identity)[self.target]["build"], info)

    def catalog(self, **changed):
        source = self.root / "source"
        source.mkdir()
        originals = {}
        for target in server_package.TARGETS.values():
            archive, signature, info = self.artifact(target, **changed)
            directory = source / target
            directory.mkdir()
            shutil.copyfile(archive, directory / "artifact.tar.gz")
            shutil.copyfile(signature, directory / "artifact.sig")
            (directory / "artifact.sha256").write_text(release.sha256(archive) + "\n")
            originals[target] = (archive, signature, info)
        return source, originals

    def test_supplied_catalog_keeps_both_targets_and_independent_server_versions(self):
        source, originals = self.catalog(commit="b" * 40, version="dev.bbbbbbbbbbbb")
        local, signature, _ = self.artifact(self.target)
        destination = self.root / "catalog"
        staged = stage_catalog(local, signature, destination, self.identity, source=source)
        self.assertEqual(set(staged), set(server_package.TARGETS.values()))
        for target, (archive, signed, info) in originals.items():
            self.assertEqual(
                (destination / target / "artifact.tar.gz").read_bytes(), archive.read_bytes()
            )
            self.assertEqual(
                (destination / target / "artifact.sig").read_bytes(), signed.read_bytes()
            )
            self.assertEqual(staged[target]["build"], info)
        self.assertEqual(verify_catalog(destination, self.identity, require_all=True), staged)

    def test_release_and_explicit_catalogs_refuse_missing_targets(self):
        archive, signature, _ = self.artifact(self.target)
        with self.assertRaisesRegex(TaskError, "complete signed server catalog"):
            stage_catalog(
                archive, signature, self.root / "release", self.identity, require_all=True
            )
        source = self.root / "source"
        stage_catalog(archive, signature, source, self.identity)
        with self.assertRaisesRegex(TaskError, "every supported native target"):
            stage_catalog(archive, signature, self.root / "missing", self.identity, source=source)
        self.assertFalse((self.root / "missing").exists())

    def test_explicit_native_entry_must_match_its_executable_even_when_local_pin_differs(self):
        source, originals = self.catalog(commit="b" * 40, version="dev.bbbbbbbbbbbb")
        archive, signature, info = originals[self.target]
        tree = archive.parent / "server"
        (tree / "build.json").write_text(
            json.dumps({**info, "commit": "c" * 40, "version": "dev.cccccccccccc"})
        )
        write_archive(tree, archive)
        release.sign_ed25519(SEED, archive, signature)
        target = source / self.target
        shutil.copyfile(archive, target / "artifact.tar.gz")
        shutil.copyfile(signature, target / "artifact.sig")
        (target / "artifact.sha256").write_text(release.sha256(archive) + "\n")
        local, local_signature, _ = self.artifact(self.target)
        with self.assertRaisesRegex(TaskError, "executable differs"):
            stage_catalog(
                local, local_signature, self.root / "refused", self.identity, source=source
            )
        self.assertFalse((self.root / "refused").exists())

    def test_foreign_terminfo_accepts_extended_numbers_and_refuses_truncated_tables(self):
        source, originals = self.catalog()
        target = next(value for value in originals if value != self.target)
        archive, signature, _ = originals[target]
        tree = archive.parent / "server"
        names = b"xterm-spaceterm\0"
        data = (
            struct.pack("<6h", 0x021E, len(names), 0, 1, 0, 0) + names + struct.pack("<i", 100000)
        )
        terminfo = next((tree / "share/spaceterm/terminfo").rglob("xterm-spaceterm"))
        local, local_signature, _ = self.artifact(self.target)
        for truncated in (False, True):
            terminfo.write_bytes(data[:-1] if truncated else data)
            write_archive(tree, archive)
            release.sign_ed25519(SEED, archive, signature)
            shutil.copyfile(archive, source / target / "artifact.tar.gz")
            shutil.copyfile(signature, source / target / "artifact.sig")
            (source / target / "artifact.sha256").write_text(release.sha256(archive) + "\n")
            destination = self.root / ("refused" if truncated else "accepted")
            if truncated:
                with self.assertRaisesRegex(TaskError, "terminfo"):
                    stage_catalog(local, local_signature, destination, self.identity, source=source)
                self.assertFalse(destination.exists())
            else:
                stage_catalog(local, local_signature, destination, self.identity, source=source)

    def test_signed_incompatible_identity_target_protocol_or_capability_is_refused(self):
        for changed in [
            {"identity": "spaceterm-development"},
            {
                "target": next(
                    target for target in server_package.TARGETS.values() if target != self.target
                )
            },
            {"protocol_major": 2},
            {"capabilities": []},
        ]:
            with self.subTest(changed=changed):
                archive, signature, _ = self.artifact(self.target, **changed)
                destination = self.root / "refused"
                with self.assertRaises(TaskError):
                    stage_catalog(archive, signature, destination, self.identity)
                self.assertFalse(destination.exists())

    def test_signed_machine_or_resource_substitution_is_refused(self):
        for corrupt_machine in (False, True):
            with self.subTest(corrupt_machine=corrupt_machine):
                archive, signature, _ = self.artifact(self.target)
                tree = archive.parent / "server"
                if corrupt_machine:
                    (tree / "bin/spaceterm-server").write_bytes(b"wrong native machine")
                else:
                    (tree / "share/spaceterm/shell-integration/zsh/.zshenv").unlink()
                write_archive(tree, archive)
                release.sign_ed25519(SEED, archive, signature)
                with self.assertRaises(TaskError):
                    stage_catalog(archive, signature, self.root / "refused", self.identity)
                self.assertFalse((self.root / "refused").exists())

    def test_catalog_tampering_and_unsupported_entries_are_refused(self):
        source, originals = self.catalog()
        archive, signature, _ = originals[self.target]
        target = source / self.target
        for name, payload in [
            ("artifact.tar.gz", archive.read_bytes() + b"tampered"),
            ("artifact.sig", bytes(64)),
            ("artifact.sha256", b"0" * 64 + b"\n"),
            ("artifact.sha256", b"invalid digest"),
            ("unexpected", b"untracked"),
        ]:
            with self.subTest(name=name, payload=payload[-16:]):
                path = target / name
                saved = path.read_bytes() if path.exists() else None
                path.write_bytes(payload)
                with self.assertRaises(TaskError):
                    verify_catalog(source, self.identity, require_all=True)
                if saved is None:
                    path.unlink()
                else:
                    path.write_bytes(saved)
        (source / "unsupported-target").mkdir()
        with self.assertRaisesRegex(TaskError, "unsupported targets"):
            verify_catalog(source, self.identity)

    def test_catalog_symlinks_and_special_files_are_refused_without_following_them(self):
        source, _ = self.catalog()
        link = self.root / "link"
        link.symlink_to(source, target_is_directory=True)
        with self.assertRaises(TaskError):
            verify_catalog(link, self.identity)
        target = source / self.target
        saved_target = self.root / "saved-target"
        target.rename(saved_target)
        target.symlink_to(saved_target, target_is_directory=True)
        with self.assertRaises(TaskError):
            verify_catalog(source, self.identity)
        target.unlink()
        saved_target.rename(target)
        archive = target / "artifact.tar.gz"
        saved = self.root / "archive"
        archive.rename(saved)
        archive.symlink_to(saved)
        with self.assertRaises(TaskError):
            verify_catalog(source, self.identity)
        archive.unlink()
        os.mkfifo(archive)
        with self.assertRaises(TaskError):
            verify_catalog(source, self.identity)

    def test_concurrent_input_replacement_never_publishes_unverified_bytes(self):
        source, originals = self.catalog()
        archive, signature, _ = originals[self.target]
        path = source / self.target / "artifact.tar.gz"
        valid = archive.read_bytes()
        stop = threading.Event()

        def replace():
            while not stop.is_set():
                for contents in (valid + b"tampered", valid):
                    temporary = self.root / "replacement"
                    temporary.write_bytes(contents)
                    temporary.replace(path)

        worker = threading.Thread(target=replace)
        worker.start()
        try:
            for attempt in range(6):
                destination = self.root / f"attempt-{attempt}"
                try:
                    stage_catalog(archive, signature, destination, self.identity, source=source)
                except TaskError:
                    self.assertFalse(destination.exists())
                else:
                    self.assertEqual(
                        (destination / self.target / "artifact.tar.gz").read_bytes(), valid
                    )
        finally:
            stop.set()
            worker.join(timeout=5)
        self.assertFalse(worker.is_alive())


if __name__ == "__main__":
    unittest.main()
