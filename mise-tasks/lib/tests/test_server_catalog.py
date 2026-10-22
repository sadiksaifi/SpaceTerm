"""A packaged SSH catalog retains only authenticated target-specific server bytes."""

import os
import shutil
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import ROOT, TaskError, release, server_package
from spaceterm_tasks.package_linux import write_archive
from spaceterm_tasks.server_catalog import stage_catalog, verify_catalog

from tests.server_artifact_fixture import SEED, archive_tree


class ServerCatalogTests(unittest.TestCase):
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
        executable.write_bytes(header)
        executable.chmod(0o755)
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
