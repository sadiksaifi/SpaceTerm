#!/usr/bin/env python3
"""Run the release installer against a served fixture release and a real disk image."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

INSTALLER = Path(__file__).resolve().with_name("install-release.sh")
RELEASES = "https://github.com/sadiksaifi/SpaceTerm/releases"
NOTICE = "By using SpaceTerm, you acknowledge that it's not notarized."


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="spaceterm-install-test-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.applications = self.root / "Applications with spaces"
        self.temporary = self.root / "tmp"
        self.temporary.mkdir()
        self.served = {}
        self.requests = self.root / "requests.log"

        # The network is the controlled external boundary. Disk image mounting, copying, and
        # process checks use the real macOS tools.
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.bin / "curl").write_text(f"""#!{sys.executable}
import json, os, shutil, sys
arguments = sys.argv[1:]
url = next(argument for argument in arguments if argument.startswith("https://"))
with open(os.environ["REQUESTS"], "a") as log:
    log.write(url + "\\n")
source = json.loads(os.environ["SERVED"]).get(url)
if source is None:
    sys.exit(22)
shutil.copyfile(source, arguments[arguments.index("--output") + 1])
""")
        (self.bin / "curl").chmod(0o755)

        self.archive = self.disk_image("0.3.0")
        self.serve("latest/download/SHA256SUMS", self.checksums(self.archive))
        self.serve("download/v0.3.0/SpaceTerm-0.3.0-darwin-arm64.dmg", self.archive)

    def application(self, path, version):
        executable = path / "Contents/MacOS/SpaceTerm"
        executable.parent.mkdir(parents=True)
        executable.write_text("#!/bin/sh\nexec sleep 30\n")
        executable.chmod(0o755)
        (path / "Contents/version").write_text(version)

    def disk_image(self, version):
        source = self.root / f"disk-{version}"
        self.application(source / "SpaceTerm.app", version)
        (source / "Applications").symlink_to("/Applications")
        archive = self.root / f"SpaceTerm-{version}-darwin-arm64.dmg"
        subprocess.run(["hdiutil", "create", "-quiet", "-srcfolder", str(source), "-format", "UDZO",
                        str(archive)], check=True)
        return archive

    def checksums(self, archive):
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        path = self.root / "SHA256SUMS"
        path.write_text(f"{digest}  {archive.name}\n{'0' * 64}  appcast.xml\n")
        return path

    def serve(self, path, source):
        self.served[f"{RELEASES}/{path}"] = str(source)

    def install(self):
        environment = {**os.environ, "PATH": f"{self.bin}:{os.environ['PATH']}",
                       "SERVED": json.dumps(self.served), "REQUESTS": str(self.requests),
                       "SPACETERM_INSTALL_DIR": str(self.applications), "TMPDIR": str(self.temporary)}
        return subprocess.run(["sh", str(INSTALLER)], env=environment, capture_output=True, text=True,
                              timeout=120)

    def installed_version(self):
        return (self.applications / "SpaceTerm.app/Contents/version").read_text()

    def requested(self):
        return self.requests.read_text().splitlines() if self.requests.exists() else []

    def assert_no_leftovers(self):
        self.assertEqual(list(self.temporary.iterdir()), [])
        self.assertEqual(sorted(path.name for path in self.applications.iterdir()), ["SpaceTerm.app"])

    def test_installer_replaces_the_application_with_the_latest_checksummed_release(self):
        self.application(self.applications / "SpaceTerm.app", "0.2.0")
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.installed_version(), "0.3.0")
        self.assertEqual(self.requested(), [f"{RELEASES}/latest/download/SHA256SUMS",
                                            f"{RELEASES}/download/v0.3.0/SpaceTerm-0.3.0-darwin-arm64.dmg"])
        self.assertEqual(result.stdout.splitlines()[-1], NOTICE)
        self.assert_no_leftovers()

    def test_installer_keeps_the_existing_application_when_the_checksum_differs(self):
        self.application(self.applications / "SpaceTerm.app", "0.2.0")
        mismatched = self.root / "mismatched-SHA256SUMS"
        mismatched.write_text(f"{'f' * 64}  {self.archive.name}\n")
        self.serve("latest/download/SHA256SUMS", mismatched)
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not match its published checksum", result.stderr)
        self.assertEqual(self.installed_version(), "0.2.0")
        self.assert_no_leftovers()

    def test_installer_refuses_to_replace_a_running_application_before_downloading(self):
        self.application(self.applications / "SpaceTerm.app", "0.2.0")
        running = subprocess.Popen([str(self.applications / "SpaceTerm.app/Contents/MacOS/SpaceTerm")])
        self.addCleanup(running.wait)
        self.addCleanup(running.kill)
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("quit SpaceTerm", result.stderr)
        self.assertEqual(self.requested(), [])
        self.assertEqual(self.installed_version(), "0.2.0")


if __name__ == "__main__":
    unittest.main()
