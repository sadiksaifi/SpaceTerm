"""The Homebrew cask refers only to the notarized greenfield disk image."""

import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import TaskError, release_feed, release_homebrew, update_signing


class CaskTests(unittest.TestCase):
    def test_cask_uses_the_immutable_dmg_and_new_application_zap_paths(self):
        text = release_homebrew.cask("0.10.0", "a" * 64)
        self.assertIn('version "0.10.0"', text)
        self.assertIn('sha256 "' + "a" * 64 + '"', text)
        self.assertIn(release_feed.dmg_name("0.10.0").replace("0.10.0", "#{version}"), text)
        self.assertIn('app "SpaceTerm.app"', text)
        self.assertIn("com.peblon.spaceterm.plist", text)
        for removed in (
            "quarantine",
            "postflight",
            "not notarized",
            "caveats",
            "io.github.sadiksaifi",
        ):
            self.assertNotIn(removed, text)

    def test_cask_refuses_legacy_versions_and_malformed_digests(self):
        for version, digest in (("0.6.8", "a" * 64), ("0.10.0", "a" * 63), ("0.10.0", "x" * 64)):
            with self.subTest(version=version, digest=digest), self.assertRaises(TaskError):
                release_homebrew.cask(version, digest)

    def test_cask_takes_the_digest_of_the_assembled_disk_image(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory, tap = Path(temporary) / "release", Path(temporary) / "tap"
            directory.mkdir()
            with self.assertRaises(TaskError):
                release_homebrew.write("v0.10.0", tap, directory)
            image = directory / release_feed.dmg_name("0.10.0")
            image.write_bytes(b"notarized disk image")
            release_homebrew.write("v0.10.0", tap, directory)
            self.assertEqual(
                (tap / "Casks/spaceterm.rb").read_text(),
                release_homebrew.cask("0.10.0", update_signing.sha256(image)),
            )
