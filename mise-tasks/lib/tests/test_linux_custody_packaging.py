"""Test the APT prerequisite's exact bytes, ownership, and lack of installation effects."""

import io
import json
import os
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import TaskError, package_server, package_server_linux
from spaceterm_tasks import linux_custody_packaging as publisher
from spaceterm_tasks import server_artifacts as artifacts
from spaceterm_tasks.macos_signing import SIGNING_INPUTS

from tests import test_package_server as fixtures
from tests.test_package_server import metadata


class PublisherPackageTests(unittest.TestCase):
    def fixture(self, temporary):
        root = Path(temporary) / "spaceterm-server"
        root.mkdir()
        names = [
            *artifacts.entry_points("linux-x86_64", "preflight").values(),
            "libexec/spaceterm-server-spawner",
        ]
        for name in names:
            if name is not None:
                file = root / name
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_bytes(name.encode())
                file.chmod(0o501 if name == "bin/spaceterm-server" else 0o555)
        package_server.write_manifest(root, metadata(build="preflight"))
        return root

    def test_publisher_copies_authenticated_bytes_and_has_no_privileged_scripts(self):
        previous = os.umask(0o077)
        self.addCleanup(os.umask, previous)
        with tempfile.TemporaryDirectory() as temporary:
            root = self.fixture(temporary)
            destination = Path(temporary) / "candidate.deb"
            calls = []

            def dpkg(command, _failure, **kwargs):
                calls.append(command)
                self.assertEqual(
                    command[:4], ["dpkg-deb", "--build", "--root-owner-group", "-Zgzip"]
                )
                self.assertFalse(set(SIGNING_INPUTS) & set(kwargs["env"]))
                stage, output = command[-2:]
                self.assertEqual([p.name for p in (stage / "DEBIAN").iterdir()], ["control"])
                self.assertEqual((stage / "DEBIAN").stat().st_mode & 0o777, 0o755)
                installed = stage / "usr/lib/spaceterm-native-publisher-preflight"
                self.assertEqual(
                    (installed / "installation.json").read_bytes(),
                    (root / "installation.json").read_bytes(),
                )
                self.assertEqual((installed / "bin/spaceterm-server").stat().st_mode & 0o777, 0o501)
                self.assertTrue((installed / "libexec/spaceterm-server-spawner").is_file())
                output.write_bytes(b"injected dpkg output")

            with (
                patch.object(publisher, "checked", side_effect=dpkg),
                patch.dict(os.environ, dict.fromkeys(SIGNING_INPUTS, "sentinel-signing-input")),
            ):
                self.assertEqual(publisher.build_publisher_package(root, destination), destination)
            self.assertEqual(len(calls), 1)
            with patch.object(publisher, "checked") as native, self.assertRaises(TaskError):
                publisher.build_publisher_package(root, destination)
            native.assert_not_called()

    def test_substituted_files_reject_before_package_construction(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = self.fixture(temporary)
            spawner = root / "libexec/spaceterm-server-spawner"
            spawner.chmod(0o755)
            spawner.write_bytes(b"substitution")
            with patch.object(publisher, "checked") as native, self.assertRaises(TaskError):
                publisher.build_publisher_package(root, Path(temporary) / "candidate.deb")
            native.assert_not_called()

    def test_evidence_candidate_is_separate_unrun_and_binds_every_artifact(self):
        import copy

        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            closure, pin, binaries, _ = fixtures.CredentialClosureStagingTests().fixture(temporary)
            pin = copy.deepcopy(pin)
            pin["runtime"].update(
                jammy_result="unrun", host_key_result="unrun", tpm_result="unrun", evidence={}
            )
            native_calls = []

            def dpkg(command, _failure, **_kwargs):
                native_calls.append(command)
                command[-1].write_bytes(b"injected dpkg output")

            with (
                patch.object(package_server_linux.sys, "platform", "linux"),
                patch.object(package_server_linux, "DIST", base),
                patch.object(package_server_linux, "CLOSURE", closure),
                patch.object(package_server_linux, "credential_tool_pin", return_value=pin),
                patch.object(package_server_linux, "verify_elf_closure"),
                patch.object(package_server, "build", return_value=binaries),
                patch.object(
                    package_server, "executable_metadata", return_value=metadata(build="preflight")
                ),
                patch.object(publisher, "checked", side_effect=dpkg),
            ):
                archive = package_server_linux.package_evidence_candidate()
                with self.assertRaises(TaskError):
                    package_server_linux.package_evidence_candidate()
                with self.assertRaisesRegex(TaskError, "Jammy evidence are unavailable"):
                    package_server_linux.package()
            self.assertEqual(len(native_calls), 1)
            self.assertEqual(archive.name, "spaceterm-server-evidence-linux-x86_64.tar.gz")
            self.assertFalse((base / "spaceterm-server-preflight-linux-x86_64.tar.gz").exists())
            receipt = json.loads(archive.with_name(archive.name + ".json").read_bytes())
            self.assertEqual(receipt["purpose"], "native-evidence-candidate")
            self.assertEqual(receipt["runtime_evidence"], "unrun")
            self.assertEqual(receipt["credential_closure_sha256"], pin["runtime"]["closure_sha256"])
            self.assertEqual(receipt["archive_sha256"], artifacts.digest(archive))
            self.assertEqual(
                receipt["publisher_package_sha256"],
                artifacts.digest(base / "spaceterm-native-publisher-evidence-linux-x86_64.deb"),
            )
            manifest = package_server.verify_archive(archive)
            spawner = next(
                file
                for file in manifest["files"]
                if file["name"] == "libexec/spaceterm-server-spawner"
            )
            self.assertEqual(
                spawner["sha256"], artifacts.digest(binaries["spaceterm-server-spawner"])
            )
            with tarfile.open(archive) as package:
                stream = package.extractfile("spaceterm-server/installation.json")
                assert stream is not None
                import hashlib

                self.assertEqual(
                    receipt["manifest_sha256"], hashlib.sha256(stream.read()).hexdigest()
                )


if sys.platform == "linux":

    class NativePublisherPackageTests(unittest.TestCase):
        def test_actual_debian_package_has_root_ownership_and_no_maintainer_scripts(self):
            with tempfile.TemporaryDirectory() as temporary:
                root = PublisherPackageTests().fixture(temporary)
                output = Path(temporary) / "publisher.deb"
                publisher.build_publisher_package(root, output)
                result = subprocess.run(
                    ["dpkg-deb", "--ctrl-tarfile", output], env={}, check=True, capture_output=True
                )
                with tarfile.open(fileobj=io.BytesIO(result.stdout)) as archive:
                    self.assertEqual(
                        [entry.name for entry in archive if entry.isfile()], ["./control"]
                    )
                result = subprocess.run(
                    ["dpkg-deb", "--fsys-tarfile", output], env={}, check=True, capture_output=True
                )
                with tarfile.open(fileobj=io.BytesIO(result.stdout)) as archive:
                    for entry in archive:
                        self.assertEqual((entry.uid, entry.gid), (0, 0))
                    server = archive.getmember(
                        "./usr/lib/spaceterm-native-publisher-preflight/bin/spaceterm-server"
                    )
                    self.assertEqual(server.mode, 0o501)
                    self.assertTrue(
                        archive.getmember(
                            "./usr/lib/spaceterm-native-publisher-preflight/libexec/spaceterm-server-spawner"
                        ).isfile()
                    )


if __name__ == "__main__":
    unittest.main()
