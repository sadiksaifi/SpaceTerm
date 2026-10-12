#!/usr/bin/env python3
"""Check the real publisher metadata prerequisite without native admission or test adapters."""

import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "mise-tasks/lib"))

from spaceterm_tasks import package_server


class ActualServerMetadataTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.binaries = package_server.build()
        cls.platform = "darwin-arm64" if sys.platform == "darwin" else "linux-x86_64"

    def test_actual_server_metadata_is_available_before_native_admission(self):
        metadata = package_server.executable_metadata(
            self.binaries["spaceterm-server"], self.platform, "preflight"
        )
        self.assertEqual(metadata["component"], "server")
        self.assertEqual(metadata["build_identity"], "preflight")
        self.assertEqual(metadata["platform"], self.platform)

    def test_ordinary_server_startup_remains_fail_closed(self):
        for arguments in ([], ["--distribution-metadata", "unexpected-argument"]):
            with self.subTest(arguments=arguments):
                result = subprocess.run(
                    [self.binaries["spaceterm-server"], *arguments],
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.DEVNULL,
                    env={},
                    timeout=5,
                    check=False,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(len(result.stdout), 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
