#!/usr/bin/env python
# MISE description="Verify the Linux installer refuses macOS before downloading or changing files"
"""macOS installs through its notarized DMG or Homebrew cask."""

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from tests.installer_fixture import CURL, fake, render


@unittest.skipUnless(sys.platform == "darwin", "requires macOS")
class InstallTests(unittest.TestCase):
    def test_install_and_uninstall_refuse_macos_without_side_effects(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            home = root / "home"
            home.mkdir()
            requests = root / "requests"
            fake(root, "curl", CURL)
            installer = render(root)
            for arguments in ((), ("--desktop",), ("--uninstall",), ("--uninstall", "--desktop")):
                with self.subTest(arguments=arguments):
                    result = subprocess.run(
                        ["sh", installer, *arguments],
                        env={
                            **os.environ,
                            "HOME": str(home),
                            "PATH": f"{root}:/usr/bin:/bin",
                            "REQUESTS": str(requests),
                        },
                        capture_output=True,
                        text=True,
                        check=False,
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("requires Linux", result.stderr)
                    self.assertIn("notarized DMG or Homebrew cask", result.stderr)
                    self.assertFalse(requests.exists())
                    self.assertEqual(list(home.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
