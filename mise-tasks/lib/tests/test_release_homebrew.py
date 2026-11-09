"""The Homebrew cask refers only to the notarized greenfield disk image."""

import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import TaskError, release_feed, release_homebrew


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

    def test_cask_quits_the_app_on_uninstall_and_zap_stops_spaceterm_core(self):
        text = release_homebrew.cask("0.10.0", "a" * 64)
        self.assertIn('uninstall quit: "com.peblon.spaceterm"\n', text)
        zap = text[text.index("  zap ") :]
        # Homebrew's launchctl directive unloads each agent and removes its LaunchAgents plist.
        for label in ("com.peblon.spaceterm.core", "com.peblon.spaceterm.core.recovery"):
            self.assertIn(f'"{label}",', zap[: zap.index("delete:")])
        self.assertIn('delete:    "~/.local/bin/spaceterm",', zap)
        self.assertIn('"~/.local/share/spaceterm",', zap[zap.index("trash:") :])

    def test_cask_refuses_legacy_versions_and_malformed_digests(self):
        for version, digest in (("0.6.8", "a" * 64), ("0.10.0", "a" * 63), ("0.10.0", "x" * 64)):
            with self.subTest(version=version, digest=digest), self.assertRaises(TaskError):
                release_homebrew.cask(version, digest)

    def test_cask_takes_the_digest_of_the_published_disk_image(self):
        # A later attempt rebuilds a disk image with other bytes; users download the published one.
        name = release_feed.dmg_name("0.10.0")
        published = {"id": 7, "draft": False, "immutable": True, "tag_name": "v0.10.0"}
        assets = [{"name": name, "digest": "sha256:" + "b" * 64}, {"name": "SHA256SUMS"}]

        def request(endpoint):
            if endpoint.endswith("/releases/tags/v0.10.0"):
                return published
            if "/releases/7/assets?" in endpoint:
                return assets
            raise AssertionError(endpoint)

        with tempfile.TemporaryDirectory() as temporary:
            tap = Path(temporary) / "tap"
            release_homebrew.write("v0.10.0", tap, request=request)
            self.assertEqual(
                (tap / "Casks/spaceterm.rb").read_text(), release_homebrew.cask("0.10.0", "b" * 64)
            )
            for changed in ({"draft": True}, {"immutable": False}):
                with self.subTest(changed=changed), self.assertRaises(TaskError):
                    published.update(changed)
                    release_homebrew.write("v0.10.0", tap, request=request)
                published.update(draft=False, immutable=True)
