#!/usr/bin/env python3
"""Test release provenance through real isolated Git repositories."""

import importlib.util
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("release_version", Path(__file__).with_name("release-version.py"))
VERSION = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERSION)
ARTIFACT_SPEC = importlib.util.spec_from_file_location("release_artifacts", Path(__file__).with_name("release-artifacts-macos.py"))
ARTIFACTS = importlib.util.module_from_spec(ARTIFACT_SPEC)
ARTIFACT_SPEC.loader.exec_module(ARTIFACTS)
PUBLISH_SPEC = importlib.util.spec_from_file_location("publish_release", Path(__file__).with_name("publish-release.py"))
PUBLISH = importlib.util.module_from_spec(PUBLISH_SPEC)
PUBLISH_SPEC.loader.exec_module(PUBLISH)


class ReleaseVersionTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.git("init", "-q")
        self.git("config", "user.email", "test@example.invalid")
        self.git("config", "user.name", "Test")
        self.git("commit", "-qm", "Initial", "--allow-empty")

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.root), *args], check=True, capture_output=True, text=True).stdout.strip()

    def test_release_uses_the_annotated_tag_on_the_build_commit(self):
        self.git("tag", "-a", "v0.1.0", "-m", "Release")
        identity = VERSION.resolve(self.root, "v0.1.0", require_clean=True)
        self.assertEqual((identity["version"], identity["bundle_version"], identity["commit"]),
                         ("0.1.0", "0.1.0", self.git("rev-parse", "HEAD")))

    def test_release_rejects_lightweight_tags_and_tags_on_other_commits(self):
        self.git("tag", "v0.1.0")
        with self.assertRaises(VERSION.VersionError):
            VERSION.resolve(self.root, "v0.1.0")
        self.git("tag", "-a", "v0.2.0", "-m", "Release")
        self.git("commit", "-qm", "Next", "--allow-empty")
        with self.assertRaises(VERSION.VersionError):
            VERSION.resolve(self.root, "v0.2.0")

    def test_release_rejects_dirty_builds_and_noncanonical_versions(self):
        self.git("tag", "-a", "v0.1.0", "-m", "Release")
        (self.root / "changed").write_text("dirty")
        for tag in ("0.1.0", "v0.1.0", "v0.1.0-beta.1", "v01.1.0", "v0.1.0+build"):
            with self.subTest(tag=tag), self.assertRaises(VERSION.VersionError):
                VERSION.resolve(self.root, tag, require_clean=True)

    def test_development_build_is_never_a_release(self):
        self.git("tag", "-a", "v0.1.0", "-m", "Release")
        self.assertEqual(VERSION.resolve(self.root)["version"], f"dev.{self.git('rev-parse', 'HEAD')[:12]}")

    def test_publisher_uploads_version_named_assets_before_publishing_the_tag(self):
        directory = self.root / "dist/release"
        directory.mkdir(parents=True)
        assets = [directory / name for name in ("SpaceTerm-0.1.0-darwin-arm64.dmg", "appcast.xml", "SHA256SUMS")]
        for asset in assets:
            asset.write_bytes(b"fixture")
        calls = []

        def github(*args, **kwargs):
            calls.append(args)
            return subprocess.CompletedProcess(args, 1 if args[:2] == ("release", "view") else 0, "", "")

        with patch.object(PUBLISH, "ROOT", self.root), patch.object(PUBLISH, "gh", github), patch("sys.argv", ["publish-release.py", "v0.1.0"]):
            PUBLISH.main()
        self.assertEqual([args[:2] for args in calls], [("release", "view"), ("release", "create"), ("release", "upload"), ("release", "edit")])
        self.assertEqual(calls[2], ("release", "upload", "v0.1.0", *(str(asset) for asset in assets), "--clobber"))
        self.assertIn("--draft", calls[1])
        self.assertIn("--draft=false", calls[3])

    def test_feed_must_reference_the_exact_release_asset_and_version(self):
        import base64
        archive = self.root / "SpaceTerm-0.1.0-darwin-arm64.dmg"
        archive.write_bytes(b"fixture")
        signature = base64.b64encode(bytes(64)).decode()
        xml = f'<rss xmlns:sparkle="{ARTIFACTS.SPARKLE_NS}"><channel><item><sparkle:version>0.1.0</sparkle:version><sparkle:shortVersionString>0.1.0</sparkle:shortVersionString><enclosure url="https://github.com/sadiksaifi/SpaceTerm/releases/download/v0.1.0/{archive.name}" length="7" sparkle:edSignature="{signature}" /></item></channel></rss>'
        feed = self.root / "appcast.xml"
        feed.write_text(xml)
        self.assertEqual(ARTIFACTS.verify_feed(feed, archive, "0.1.0"), signature)
        for invalid in (xml.replace("github.com/", "example.invalid/"), xml.replace("<sparkle:version>0.1.0", "<sparkle:version>0.2.0"), xml.replace('length="7"', 'length="8"')):
            feed.write_text(invalid)
            with self.assertRaises(ValueError):
                ARTIFACTS.verify_feed(feed, archive, "0.1.0")


if __name__ == "__main__":
    unittest.main()
