"""Pinned native build inputs are verified before publishing a reusable sysroot."""

import hashlib
import io
import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import TaskError, linux_toolchain


class LinuxToolchainTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.manifest = self.root / "toolchain.json"
        self.cache = self.root / "cache"

    def manifest_for(self, payload):
        self.manifest.write_text(
            json.dumps(
                {
                    "snapshot": "https://example.invalid/ubuntu/",
                    "packages": [
                        {
                            "name": "fixture",
                            "version": "1.0",
                            "path": "pool/fixture.deb",
                            "sha256": hashlib.sha256(payload).hexdigest(),
                        }
                    ],
                }
            )
        )

    def test_changed_package_bytes_are_refused_without_publishing_a_sysroot(self):
        self.manifest_for(b"approved bytes")
        with (
            patch.object(
                linux_toolchain.urllib.request, "urlopen", return_value=io.BytesIO(b"changed")
            ),
            self.assertRaisesRegex(TaskError, "checksum"),
        ):
            linux_toolchain.prepare(self.manifest, self.cache)
        self.assertEqual(list(self.cache.glob("*/")), [])

    @unittest.skipUnless(shutil.which("dpkg-deb"), "native Debian archive extraction")
    def test_verified_packages_are_relocated_and_reused_without_another_download(self):
        package = self.root / "package"
        (package / "DEBIAN").mkdir(parents=True)
        (package / "DEBIAN/control").write_text(
            "Package: fixture\nVersion: 1.0\nArchitecture: all\n"
            "Maintainer: Fixture <fixture@example.invalid>\nDescription: fixture\n"
        )
        (package / "usr/include").mkdir(parents=True)
        (package / "usr/include/fixture.h").write_text("/* pinned header */\n")
        (package / "lib64").mkdir()
        (package / "lib64/loader").symlink_to("/usr/include/fixture.h")
        archive = self.root / "fixture.deb"
        subprocess.run(
            ["dpkg-deb", "--build", str(package), str(archive)], check=True, capture_output=True
        )
        self.manifest_for(archive.read_bytes())
        with patch.object(
            linux_toolchain.urllib.request, "urlopen", return_value=io.BytesIO(archive.read_bytes())
        ) as download:
            sysroot = linux_toolchain.prepare(self.manifest, self.cache)
            reused = linux_toolchain.prepare(self.manifest, self.cache)
        self.assertEqual((sysroot / "lib64/loader").read_text(), "/* pinned header */\n")
        self.assertEqual(reused, sysroot)
        download.assert_called_once()
