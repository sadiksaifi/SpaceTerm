"""Signed server archives are authenticated before layout or executable inspection."""

import io
import json
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import TaskError, release, server_package


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

    def test_signed_malformed_build_document_is_refused(self):
        for payload in [b"null", b"[]", b"{}", b"invalid", b'{"schema":1,"schema":2}']:
            self.archive_with([("server/build.json", payload, tarfile.REGTYPE)])
            with self.assertRaises(TaskError):
                self.unpack()


if __name__ == "__main__":
    unittest.main()
