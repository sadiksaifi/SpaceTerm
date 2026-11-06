"""install.sh's release contract and its refusals before it touches the network or the disk."""

import json
import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import ROOT

from tests.installer_fixture import fake, render

INSTALLER = ROOT / "packaging/install.sh"
CURL = f"""#!{sys.executable}
import os, sys
with open(os.environ["REQUESTS"], "a") as log:
    log.write(" ".join(sys.argv[1:]) + "\\n")
sys.exit(22)
"""


class InstallerContractTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="spaceterm-installer-contract-")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.requests = self.root / "requests.log"
        fake(self.bin, "curl", CURL)
        fake(self.bin, "uname", "#!/bin/sh\necho Linux\n")

    def run_installer(self, installer, *arguments):
        return subprocess.run(
            ["sh", str(installer), *arguments],
            env={
                "HOME": str(self.home),
                "PATH": f"{self.bin}:{os.environ['PATH']}",
                "REQUESTS": str(self.requests),
                "TMPDIR": str(self.root),
            },
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=30,
            start_new_session=True,
        )

    def assert_refused_untouched(self, result, message):
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(message, result.stderr)
        self.assertFalse(self.requests.exists())
        self.assertEqual(list(self.home.iterdir()), [])

    def test_the_release_pipeline_fills_each_value_exactly_once(self):
        text = INSTALLER.read_text()
        for token in ("@RELEASE_KIND@", "@DEFAULT_VERSION@"):
            self.assertEqual(text.count(token), 1, token)
        self.assertIn("readonly RELEASE_KIND='@RELEASE_KIND@'\n", text)
        self.assertIn("readonly DEFAULT_VERSION='@DEFAULT_VERSION@'\n", text)

    def test_the_pinned_update_keys_match_update_trust(self):
        text = INSTALLER.read_text()
        pinned = {
            name.lower(): value
            for name, value in re.findall(
                r"^readonly UPDATE_KEY_(CURRENT|BACKUP)='([^']*)'$", text, re.M
            )
        }
        self.assertEqual(pinned, json.loads((ROOT / "packaging/update-trust.json").read_text()))

    def test_an_unprepared_installer_refuses(self):
        result = self.run_installer(INSTALLER)
        self.assert_refused_untouched(result, "this installer was not prepared by a release")

    def test_a_release_without_a_pinned_version_refuses(self):
        for version in ("", "@DEFAULT_VERSION@", "v0.10.0", "0.10"):
            with self.subTest(version=version):
                installer = render(self.root, version=version)
                result = self.run_installer(installer)
                self.assert_refused_untouched(
                    result, "this installer has no pinned release version"
                )

    def test_a_release_with_placeholder_update_keys_refuses(self):
        result = self.run_installer(render(self.root, current="PLACEHOLDER", backup="PLACEHOLDER"))
        self.assert_refused_untouched(result, "this installer has placeholder update keys")

    def test_invalid_options_are_refused_before_anything_else(self):
        installer = render(self.root)
        for arguments, message in (
            (("--headless",), "unknown option: --headless"),
            (("--version",), "--version takes a version, such as 1.4.0"),
            (("--version", "v1.4.0"), "--version takes a version, such as 1.4.0"),
            (("--version", "1.04.0"), "--version takes a version, such as 1.4.0"),
            (("--version", "1.4"), "--version takes a version, such as 1.4.0"),
            (("--uninstall", "--linger"), "--uninstall takes only --desktop"),
            (("--uninstall", "--no-modify-path"), "--uninstall takes only --desktop"),
            (("--uninstall", "--version", "1.4.0"), "--uninstall takes only --desktop"),
        ):
            with self.subTest(arguments=arguments):
                result = self.run_installer(installer, *arguments)
                self.assert_refused_untouched(result, message)
                self.assertIn("Options: --desktop, --version VERSION", result.stderr)


if __name__ == "__main__":
    unittest.main()
