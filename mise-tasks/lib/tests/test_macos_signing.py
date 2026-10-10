"""Exercise native signing imports, rejection, cleanup, and cross-build identity."""

import os
import plistlib
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import TaskError
from spaceterm_tasks.macos_signing import (
    release_keychain,
    signing_environment,
    verify_release_signature,
)

from tests.macos_signing_fixture import certificate


@unittest.skipUnless(sys.platform == "darwin", "requires macOS Keychain and codesign")
class ReleaseSigningTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        temporary = tempfile.TemporaryDirectory(prefix="spaceterm-signing-test-")
        cls.addClassCleanup(temporary.cleanup)
        cls.root = Path(temporary.name)
        cls.fingerprint, cls.credentials = certificate(cls.root / "identity")

    def run_command(self, *arguments, **kwargs):
        return subprocess.run(arguments, check=True, capture_output=True, **kwargs).stdout

    def search_list(self):
        return self.run_command("security", "list-keychains", "-d", "user")

    def test_invalid_credentials_fail_without_leaving_a_keychain(self):
        original = self.search_list()
        for credentials, fingerprint in (
            ({}, self.fingerprint),
            ({**self.credentials, "MACOS_SIGNING_CERTIFICATE_P12": "not-base64"}, self.fingerprint),
            (
                {**self.credentials, "MACOS_SIGNING_CERTIFICATE_PASSWORD": "incorrect"},
                self.fingerprint,
            ),
            (self.credentials, "0" * 40),
        ):
            with self.subTest(fingerprint=fingerprint), self.assertRaises(TaskError):
                with release_keychain(fingerprint, {**os.environ, **credentials}):
                    self.fail("invalid signing inputs must not enter the packaging operation")
            self.assertEqual(self.search_list(), original)
        fingerprint, credentials = certificate(self.root / "public-only", private=False)
        with self.assertRaises(TaskError):
            with release_keychain(fingerprint, {**os.environ, **credentials}):
                self.fail("a certificate without its private key must be rejected")
        self.assertEqual(self.search_list(), original)

    def test_signing_identity_survives_rebuilds_and_cleanup_runs_on_interruption(self):
        original = self.search_list()
        environment = {**os.environ, **self.credentials}
        clean = signing_environment(environment)
        self.assertNotIn("MACOS_SIGNING_CERTIFICATE_P12", clean)
        self.assertNotIn("MACOS_SIGNING_CERTIFICATE_PASSWORD", clean)
        identifier = "io.github.sadiksaifi.spaceterm.signing-test"
        requirements = []
        hashes = []
        with self.assertRaises(KeyboardInterrupt):
            with release_keychain(self.fingerprint, environment):
                imported = self.search_list()
                self.assertNotEqual(imported, original)
                for version in (1, 2):
                    app = self.root / f"build-{version}" / "Fixture.app"
                    contents = app / "Contents"
                    binaries = contents / "MacOS"
                    binaries.mkdir(parents=True)
                    (contents / "Info.plist").write_bytes(
                        plistlib.dumps(
                            {
                                "CFBundleIdentifier": identifier,
                                "CFBundleExecutable": "Fixture",
                                "CFBundleVersion": str(version),
                                "CFBundlePackageType": "APPL",
                            }
                        )
                    )
                    self.run_command(
                        "xcrun",
                        "clang",
                        "-x",
                        "c",
                        "-",
                        "-o",
                        str(binaries / "Fixture"),
                        input=f"int main(){{return {version};}}".encode(),
                    )
                    self.run_command(
                        "codesign",
                        "--force",
                        "--sign",
                        self.fingerprint,
                        "--timestamp=none",
                        str(app),
                        env=clean,
                    )
                    verify_release_signature(app, self.fingerprint, identifier, "fixture")
                    requirements.append(
                        self.run_command("codesign", "--display", "--requirements", "-", str(app))
                    )
                    details = subprocess.run(
                        ["codesign", "--display", "--verbose=4", str(app)],
                        capture_output=True,
                        text=True,
                        check=True,
                    ).stderr
                    hashes.append(
                        next(line for line in details.splitlines() if line.startswith("CDHash="))
                    )
                self.assertEqual(requirements[0], requirements[1])
                self.assertNotEqual(hashes[0], hashes[1])
                # Even a valid root seal cannot admit an ad hoc nested executable.
                helper = binaries / "Helper"
                self.run_command(
                    "xcrun",
                    "clang",
                    "-x",
                    "c",
                    "-",
                    "-o",
                    str(helper),
                    input=b"int main(){return 0;}",
                )
                self.run_command("codesign", "--force", "--sign", "-", str(helper))
                self.run_command(
                    "codesign",
                    "--force",
                    "--sign",
                    self.fingerprint,
                    "--timestamp=none",
                    str(app),
                    env=clean,
                )
                with self.assertRaises(TaskError):
                    verify_release_signature(app, self.fingerprint, identifier, "fixture")
                self.run_command("codesign", "--force", "--sign", "-", str(app))
                with self.assertRaises(TaskError):
                    verify_release_signature(app, self.fingerprint, identifier, "fixture")
                raise KeyboardInterrupt()
        self.assertEqual(self.search_list(), original)
        for path in imported.decode().splitlines():
            if "spaceterm-release-signing-" in path:
                self.assertFalse(Path(path.strip().strip('"')).exists())
