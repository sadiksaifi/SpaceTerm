"""Homebrew promotion refers only to the notarized greenfield disk image."""

import unittest

from spaceterm_tasks import TaskError, release_feed, release_promote


class CaskTests(unittest.TestCase):
    def test_cask_uses_the_immutable_dmg_and_new_application_zap_paths(self):
        text = release_promote.cask("0.10.0", "a" * 64)
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

    def test_promotion_refuses_legacy_versions_and_malformed_digests(self):
        for version, digest in (("0.6.8", "a" * 64), ("0.10.0", "a" * 63), ("0.10.0", "x" * 64)):
            with self.subTest(version=version, digest=digest), self.assertRaises(TaskError):
                release_promote.cask(version, digest)
